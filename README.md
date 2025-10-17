# Rust Operator

Rust-based Kubernetes operator that provisions a simple static web frontend from a custom resource.  
It manages the full stack - ConfigMap (HTML), Deployment, Service, and optional Ingress—based on the `RustOperator` spec.

## Project Layout

- `src/main.rs` — entrypoint that wires tracing and boots the controller.
- `src/crd.rs` — CRD type definitions plus a helper to print the generated YAML.
- `src/controller.rs` — reconciliation logic, status updates, and finalizer handling.
- `src/resources.rs` — builders for ConfigMap/Deployment/Service/Ingress plus shared helpers.
- `k8s/base` — base Kustomize manifests: CRD, operator deployment/RBAC, sample frontend CR.
- `k8s/overlays/dev` — overlay that pins the controller image to the locally-built tag and disables pulls.

## Requirements

- Rust toolchain (edition 2024) and `cargo`
- `kubectl` ≥ 1.27 with access to a Kubernetes cluster
- `kustomize` (bundled with `kubectl >= 1.14`)
- Docker/Podman (or another OCI builder) to produce the controller image
- [`just`](https://github.com/casey/just) for the task runner (optional but recommended)

## Quick Start

1. **Build the controller image**

   ```sh
   just docker-build         # or: docker build -t rust-operator:dev .
   ```

   Push or load the image into your cluster runtime.
   When using Minikube, either run `just docker-build-minikube` or wrap the build with
   `eval "$(minikube docker-env)"` so the image is stored inside the Minikube daemon.

2. **Deploy the stack**

   ```sh
   just deploy-dev
   ```

   This applies the CRD, waits for it to become `Established`, then reapplies the overlay so the sample
   `RustOperator/site` resource can be created without the “no matches for kind” race.

3. **Verify**

   ```sh
   kubectl get pods -l app=rust-operator
   kubectl get service site-service
   ```

4. **Cleanup**

   ```sh
   just undeploy-dev
   ```

### Manual deployment (without `just`)

```sh
kubectl apply -k k8s/overlays/dev
kubectl wait --for=condition=Established --timeout=30s crd/rustoperators.rootster.xyz
kubectl apply -k k8s/overlays/dev
```

## Developing

Useful commands:

```sh
just fmt          # cargo fmt
just check        # cargo check
just print-crd    # emit CRD YAML without schemars format annotations
```

You can run the controller locally against a cluster by exporting a kubeconfig and running `cargo run`.
Pass `PRINT_CRD=1 cargo run --quiet` to print the CRD YAML to stdout.

### Testing changes in the operator

1. Edit the Rust sources.
2. `just fmt` and `just check`.
3. Rebuild/push the controller image: `just docker-build` (or `just docker-build-minikube` when using Minikube).
4. `just deploy-dev` to roll out the new image.

## CRD Reference (`rootster.xyz/v1`)

- `spec.message` — echoed into `.status.observed_message`.
- `spec.html` — HTML served via nginx (default static greeting).
- `spec.replicas` — nginx replica count.
- `spec.service_type` — `ClusterIP` (default) or `NodePort`.
- `spec.ingress_host` — optional host that triggers ingress creation.
- `spec.tls_secret_name` — optional TLS secret for the ingress.

Status fields include `ready_replicas` and a `Ready` condition updated by the controller.

## Building CRD YAML for distribution

```
PRINT_CRD=1 cargo run --quiet > k8s/base/crd.yaml
```

The helper strips schemars `format` annotations so the output is OLM-friendly.
