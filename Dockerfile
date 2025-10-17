# syntax=docker/dockerfile:1.6

FROM rust:latest AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release

# Debug: list contents
RUN ls -l target/release

# Optional strip (comment while debugging)
RUN strip target/release/rust-operator || true

FROM ubuntu:latest
WORKDIR /app

COPY --from=builder /app/target/release/rust-operator /usr/local/bin/rust-operator

ENV RUST_LOG=info
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/rust-operator"]
