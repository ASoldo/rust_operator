# Delegate shell work to bash for compatibility
set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

kustomize_overlay := "k8s/overlays/dev"
crd_name := "rustoperators.rootster.xyz"
controller_image := "rust-operator:dev"

default:
    just --list

fmt:
    cargo fmt

check:
    cargo check

build:
    cargo build

print-crd:
    PRINT_CRD=1 cargo run --quiet

docker-build: fmt check
    docker build -t {{controller_image}} .

docker-build-minikube: fmt check
    eval "$(minikube docker-env)"
    docker build -t {{controller_image}} .
    eval "$(minikube docker-env -u)"

deploy-dev:
    kubectl apply -k {{kustomize_overlay}}
    kubectl wait --for=condition=Established --timeout=30s crd/{{crd_name}}
    kubectl apply -k {{kustomize_overlay}}

undeploy-dev:
    kubectl delete -k {{kustomize_overlay}} --ignore-not-found

logs:
    kubectl logs deployment/rust-operator -n default -f
