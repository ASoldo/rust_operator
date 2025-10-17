use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use kube::{
    Api, Client, Resource, ResourceExt,
    api::{Patch, PatchParams},
    runtime::{
        controller::{Action, Controller},
        watcher::Config,
    },
};
use tracing::{error, info};

use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, Service},
    networking::v1::Ingress,
};

use crate::{
    crd::{HwCondition, RustOperator},
    resources::{
        FINALIZER, desired_configmap, desired_deployment, desired_ingress, desired_service, labels,
        upsert_condition,
    },
};

#[derive(Clone)]
struct Ctx {
    client: Client,
}

pub async fn run_operator() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let root: Api<RustOperator> = Api::all(client.clone());

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

async fn reconcile(obj: Arc<RustOperator>, ctx: Arc<Ctx>) -> Result<Action, kube::Error> {
    let ns = obj.namespace().unwrap_or_else(|| "default".into());
    let name = obj.name_any();

    if obj.meta().deletion_timestamp.is_some() {
        cleanup_children(&name, &ns, &ctx).await?;
        ensure_finalizer(&name, &ns, &ctx, false).await?;
        return Ok(Action::await_change());
    }

    ensure_finalizer(&name, &ns, &ctx, true).await?;

    let labels = labels(&name);
    let owner = obj.controller_owner_ref(&()).expect("owner ref");

    let cm_api: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
    let cm = desired_configmap(&name, &labels, &obj.spec.html, owner.clone());
    cm_api
        .patch(
            &name,
            &PatchParams::apply("rust-operator").force(),
            &Patch::Apply(&cm),
        )
        .await?;

    let deploy_api: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ns);
    let deploy = desired_deployment(&name, &labels, obj.spec.replicas, owner.clone(), &obj.spec);
    let deploy_obj = deploy_api
        .patch(
            &name,
            &PatchParams::apply("rust-operator").force(),
            &Patch::Apply(&deploy),
        )
        .await?;

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
                &name,
                &PatchParams::apply("rust-operator").force(),
                &Patch::Apply(&ing),
            )
            .await?;
    } else {
        let _ = ing_api.delete(&name, &Default::default()).await.ok();
    }

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

    let mut conditions = new_status.conditions.take().unwrap_or_default();
    upsert_condition(&mut conditions, ready_condition);
    new_status.conditions = Some(conditions);

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
