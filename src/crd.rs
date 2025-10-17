use kube::{CustomResource, CustomResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Top-level spec for the RustOperator custom resource.
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
    pub message: String,
    /// Inline HTML -> ConfigMap index.html
    #[serde(default)]
    pub html: String,
    /// nginx replicas
    #[serde(default = "default_replicas")]
    pub replicas: i32,
    /// "ClusterIP" or "NodePort"
    #[serde(default = "default_service_type")]
    pub service_type: String,
    /// Optional Ingress host. If set, an Ingress will be created.
    #[serde(default)]
    pub ingress_host: String,
    /// Optional TLS secret name for the Ingress
    #[serde(default)]
    pub tls_secret_name: String,
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
    pub type_: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
pub struct RustOperatorStatus {
    pub observed_message: Option<String>,
    pub ready_replicas: Option<i32>,
    pub conditions: Option<Vec<HwCondition>>,
}

/// Helper to emit the CRD without schemars `format` annotations that OLM dislikes.
pub fn print_crd_without_formats() -> anyhow::Result<()> {
    let crd = RustOperator::crd();
    let mut v = serde_json::to_value(&crd)?;
    strip_format_keys(&mut v);
    println!("{}", serde_yaml::to_string(&v)?);
    Ok(())
}

fn strip_format_keys(v: &mut serde_json::Value) {
    use serde_json::Value::*;
    match v {
        Object(map) => {
            map.remove("format");
            for val in map.values_mut() {
                strip_format_keys(val);
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
