use std::collections::BTreeMap;

use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{
            ConfigMap, Container, ContainerPort, PodSpec, PodTemplateSpec, Service, ServicePort,
            ServiceSpec, Volume, VolumeMount,
        },
        networking::v1::{
            HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
            IngressServiceBackend, IngressSpec, IngressTLS, ServiceBackendPort,
        },
    },
    apimachinery::pkg::{apis::meta::v1::ObjectMeta, util::intstr::IntOrString},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::crd::RustOperatorSpec;

pub const FINALIZER: &str = "rustoperators.rootster.xyz/finalizer";

pub fn labels(name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".into(), "webapp".into()),
        ("app.kubernetes.io/instance".into(), name.into()),
    ])
}

pub fn desired_configmap(
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
}

fn rollout_fingerprint(inp: &RolloutInputs) -> String {
    let mut h = Sha256::new();
    let bytes = serde_json::to_vec(inp).expect("fingerprint serialize");
    h.update(bytes);
    format!("{:x}", h.finalize())
}

pub fn desired_deployment(
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
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
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
                        image: Some("nginx:latest".into()),
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

pub fn desired_service(
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

pub fn desired_ingress(
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

pub fn upsert_condition(list: &mut Vec<crate::crd::HwCondition>, newc: crate::crd::HwCondition) {
    if let Some(i) = list.iter().position(|c| c.type_ == newc.type_) {
        list[i] = newc;
    } else {
        list.push(newc);
    }
}
