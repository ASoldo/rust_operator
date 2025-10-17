use futures_util::StreamExt;
use kube::CustomResourceExt;
use kube::{
    Api, Client, CustomResource, Resource, ResourceExt,
    api::{Patch, PatchParams},
    runtime::{
        controller::{Action, Controller},
        watcher::Config,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tracing::{error, info};

// k8s types
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{
        ConfigMap, Container, ContainerPort, PodSpec, PodTemplateSpec, Service, ServicePort,
        ServiceSpec, Volume, VolumeMount,
    },
    networking::v1::{
        HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
        IngressServiceBackend, IngressSpec, IngressTLS, ServiceBackendPort,
    },
};
use k8s_openapi::apimachinery::pkg::{
    apis::meta::v1::{LabelSelector, ObjectMeta},
    util::intstr::IntOrString,
};

fn print_crd_without_formats() -> anyhow::Result<()> {
    // Generate the CRD as JSON value
    let crd = RustOperator::crd();
    let mut v = serde_json::to_value(&crd)?;
    // Recursively remove all "format" keys
    strip_format_keys(&mut v);
    // Emit YAML to stdout
    println!("{}", serde_yaml::to_string(&v)?);
    Ok(())
}

fn strip_format_keys(v: &mut serde_json::Value) {
    use serde_json::Value::*;
    match v {
        Object(map) => {
            map.remove("format"); // kill it at this level
            for val in map.values_mut() {
                strip_format_keys(val); // recurse
            }
        }
        Array(arr) => {
            for val in arr {
                strip_format_keys(val);
            }
        }
        _ => {}
    }
}

// --- CRD ---

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "rootster.xyz",
    version = "v1",
    kind = "RustOperator",
    plural = "rustoperators",
    namespaced
)]
#[kube(status = "RustOperatorStatus")]
pub struct RustOperatorSpec {
    /// Echoed into status
    message: String,
    /// Inline HTML -> ConfigMap index.html
    #[serde(default)]
    html: String,
    /// nginx replicas
    #[serde(default = "default_replicas")]
    replicas: i32,
    /// "ClusterIP" or "NodePort"
    #[serde(default = "default_service_type")]
    service_type: String,
    /// Optional Ingress host. If set, an Ingress will be created.
    #[serde(default)]
    ingress_host: String,
    /// Optional TLS secret name for the Ingress
    #[serde(default)]
    tls_secret_name: String,
}

fn default_replicas() -> i32 {
    1
}
fn default_service_type() -> String {
    "ClusterIP".to_string()
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
pub struct HwCondition {
    #[serde(rename = "type")]
    type_: String,
    status: String,
    reason: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
pub struct RustOperatorStatus {
    observed_message: Option<String>,
    ready_replicas: Option<i32>,
    conditions: Option<Vec<HwCondition>>,
}

// --- Controller context ---

#[derive(Clone)]
struct Ctx {
    client: Client,
}

// finalizer tag
const FINALIZER: &str = "rustoperators.rootster.xyz/finalizer";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // Dev helper: print CRD YAML then exit
    if std::env::var("PRINT_CRD").is_ok() {
        print_crd_without_formats()?;
        return Ok(());
    }

    let client = Client::try_default().await?;
    let root: Api<RustOperator> = Api::all(client.clone());

    // also watch children so their changes trigger reconciles
    let deploys: Api<Deployment> = Api::all(client.clone());
    let svcs: Api<Service> = Api::all(client.clone());
    let cms: Api<ConfigMap> = Api::all(client.clone());
    let ings: Api<Ingress> = Api::all(client.clone());

    Controller::new(root, Config::default())
        .owns(deploys, Config::default())
        .owns(svcs, Config::default())
        .owns(cms, Config::default())
        .owns(ings, Config::default())
        .run(reconcile, error_policy, Arc::new(Ctx { client }))
        .for_each(|res| async move {
            match res {
                Ok((objref, _action)) => info!("✅ reconciled {}", objref.name),
                Err(e) => error!("❌ reconcile failed: {e:?}"),
            }
        })
        .await;

    Ok(())
}

// --- Reconciler ---

async fn reconcile(obj: Arc<RustOperator>, ctx: Arc<Ctx>) -> Result<Action, kube::Error> {
    let ns = obj.namespace().unwrap_or_else(|| "default".into());
    let name = obj.name_any();

    // If deleting: cleanup children, drop finalizer, and await deletion
    if obj.meta().deletion_timestamp.is_some() {
        cleanup_children(&name, &ns, &ctx).await?;
        ensure_finalizer(&name, &ns, &ctx, /*present=*/ false).await?;
        return Ok(Action::await_change());
    }

    // Ensure finalizer present
    ensure_finalizer(&name, &ns, &ctx, /*present=*/ true).await?;

    // Desired labels/owner
    let labels = labels(&name);
    let owner = obj.controller_owner_ref(&()).expect("owner ref");

    // ConfigMap with index.html
    let cm_api: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
    let cm = desired_configmap(&name, &labels, &obj.spec.html, owner.clone());
    cm_api
        .patch(
            &name,
            &PatchParams::apply("rust-operator").force(),
            &Patch::Apply(&cm),
        )
        .await?;

    // Deployment mounting the ConfigMap (with rollout hash on restart-worthy inputs)
    let deploy_api: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ns);
    let deploy = desired_deployment(&name, &labels, obj.spec.replicas, owner.clone(), &obj.spec);
    let deploy_obj = deploy_api
        .patch(
            &name,
            &PatchParams::apply("rust-operator").force(),
            &Patch::Apply(&deploy),
        )
        .await?;

    // Service
    let svc_api: Api<Service> = Api::namespaced(ctx.client.clone(), &ns);
    let svc_name = format!("{name}-service");
    let svc = desired_service(&name, &labels, &obj.spec.service_type, owner.clone());
    svc_api
        .patch(
            &svc_name,
            &PatchParams::apply("rust-operator").force(),
            &Patch::Apply(&svc),
        )
        .await?;

    // Optional Ingress (create/patch or delete if host cleared)
    let ing_api: Api<Ingress> = Api::namespaced(ctx.client.clone(), &ns);
    if !obj.spec.ingress_host.trim().is_empty() {
        let ing = desired_ingress(
            &name,
            &labels,
            &svc_name,
            &obj.spec.ingress_host,
            &obj.spec.tls_secret_name,
            owner.clone(),
        );
        ing_api
            .patch(
                &name, // ingress shares CR name
                &PatchParams::apply("rust-operator").force(),
                &Patch::Apply(&ing),
            )
            .await?;
    } else {
        let _ = ing_api.delete(&name, &Default::default()).await.ok();
    }

    // Status: message, ready replicas, conditions
    let ready = deploy_obj
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0);

    let ready_condition = HwCondition {
        type_: "Ready".into(),
        status: if ready > 0 {
            "True".into()
        } else {
            "False".into()
        },
        reason: Some(if ready > 0 {
            "PodsAvailable".into()
        } else {
            "Scaling".into()
        }),
        message: Some(format!("ready_replicas={ready}")),
    };

    let mut new_status = obj.status.clone().unwrap_or_default();

    if new_status.observed_message.as_deref() != Some(&obj.spec.message) {
        new_status.observed_message = Some(obj.spec.message.clone());
    }
    if new_status.ready_replicas != Some(ready) {
        new_status.ready_replicas = Some(ready);
    }
    // upsert Ready condition
    let mut conditions = new_status.conditions.take().unwrap_or_default();
    upsert_condition(&mut conditions, ready_condition);
    new_status.conditions = Some(conditions);

    // Only patch if changed
    let old_status = obj.status.clone().unwrap_or_default();
    if new_status != old_status {
        let api: Api<RustOperator> = Api::namespaced(ctx.client.clone(), &ns);
        let patch = serde_json::json!({ "status": new_status });
        api.patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
    }

    Ok(Action::requeue(Duration::from_secs(30)))
}

fn error_policy(_obj: Arc<RustOperator>, err: &kube::Error, _ctx: Arc<Ctx>) -> Action {
    error!("reconcile error: {err:?}");
    Action::requeue(Duration::from_secs(10))
}

// --- Finalizer helpers ---

async fn ensure_finalizer(
    name: &str,
    ns: &str,
    ctx: &Ctx,
    present: bool,
) -> Result<(), kube::Error> {
    let api: Api<RustOperator> = Api::namespaced(ctx.client.clone(), ns);
    if present {
        let patch = serde_json::json!({ "metadata": { "finalizers": [FINALIZER] }});
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
    } else {
        let patch = serde_json::json!({ "metadata": { "finalizers": [] }});
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
    }
    Ok(())
}

async fn cleanup_children(name: &str, ns: &str, ctx: &Ctx) -> Result<(), kube::Error> {
    let deploys: Api<Deployment> = Api::namespaced(ctx.client.clone(), ns);
    let svcs: Api<Service> = Api::namespaced(ctx.client.clone(), ns);
    let cms: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), ns);
    let _ = deploys.delete(name, &Default::default()).await;
    let _ = svcs
        .delete(&format!("{name}-service"), &Default::default())
        .await;
    let _ = cms.delete(name, &Default::default()).await;
    Ok(())
}

// --- Helpers ---

fn labels(name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".into(), "webapp".into()),
        ("app.kubernetes.io/instance".into(), name.into()),
    ])
}

fn desired_configmap(
    name: &str,
    labels: &BTreeMap<String, String>,
    html: &str,
    owner: k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> ConfigMap {
    let content = if html.trim().is_empty() {
        "<!doctype html><html><body><h1>Hello from Rust operator</h1></body></html>".to_string()
    } else {
        html.to_string()
    };

    ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([("index.html".into(), content)])),
        ..Default::default()
    }
}

#[derive(Serialize)]
struct RolloutInputs<'a> {
    html: &'a str,
    // add more fields later that should trigger a rollout
}

fn rollout_fingerprint(inp: &RolloutInputs) -> String {
    let mut h = Sha256::new();
    let bytes = serde_json::to_vec(inp).expect("fingerprint serialize");
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn desired_deployment(
    name: &str,
    labels: &BTreeMap<String, String>,
    replicas: i32,
    owner: k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
    spec: &RustOperatorSpec,
) -> Deployment {
    let fp = rollout_fingerprint(&RolloutInputs { html: &spec.html });

    Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::apps::v1::DeploymentSpec {
            replicas: Some(replicas),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    annotations: Some(BTreeMap::from([(
                        "rootster.xyz/rollout-hash".to_string(),
                        fp,
                    )])),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "nginx".into(),
                        image: Some("nginx:1.27-alpine".into()),
                        ports: Some(vec![ContainerPort {
                            container_port: 80,
                            ..Default::default()
                        }]),
                        volume_mounts: Some(vec![VolumeMount {
                            name: "html".into(),
                            mount_path: "/usr/share/nginx/html".into(),
                            read_only: Some(true),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    volumes: Some(vec![Volume {
                        name: "html".into(),
                        config_map: Some(k8s_openapi::api::core::v1::ConfigMapVolumeSource {
                            name: name.to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn desired_service(
    name: &str,
    labels: &BTreeMap<String, String>,
    svc_type: &str,
    owner: k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> Service {
    Service {
        metadata: ObjectMeta {
            name: Some(format!("{name}-service")),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(labels.clone()),
            ports: Some(vec![ServicePort {
                port: 80,
                target_port: Some(IntOrString::Int(80)),
                ..Default::default()
            }]),
            type_: Some(svc_type.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn desired_ingress(
    name: &str,
    labels: &BTreeMap<String, String>,
    svc_name: &str,
    host: &str,
    tls_secret: &str,
    owner: k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> Ingress {
    let backend = IngressBackend {
        service: Some(IngressServiceBackend {
            name: svc_name.to_string(),
            port: Some(ServiceBackendPort {
                number: Some(80),
                name: None,
            }),
        }),
        resource: None,
    };

    let path = HTTPIngressPath {
        backend,
        path: Some("/".into()),
        path_type: "Prefix".into(),
    };

    let rule = IngressRule {
        host: Some(host.to_string()),
        http: Some(HTTPIngressRuleValue { paths: vec![path] }),
    };

    let tls = if tls_secret.is_empty() {
        None
    } else {
        Some(vec![IngressTLS {
            hosts: Some(vec![host.to_string()]),
            secret_name: Some(tls_secret.to_string()),
        }])
    };

    Ingress {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: Some(IngressSpec {
            ingress_class_name: None,
            rules: Some(vec![rule]),
            tls,
            ..Default::default()
        }),
        ..Default::default()
    }
}

// condition upsert helper
fn upsert_condition(list: &mut Vec<HwCondition>, newc: HwCondition) {
    if let Some(i) = list.iter().position(|c| c.type_ == newc.type_) {
        list[i] = newc;
    } else {
        list.push(newc);
    }
}
