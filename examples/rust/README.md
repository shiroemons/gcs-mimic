# Rust example

Uses the official [`google-cloud-storage`](https://crates.io/crates/google-cloud-storage)
crate (1.21.0) against a local gcs-mimic: it creates a bucket, uploads a text
object, lists the bucket, downloads the object and prints it, then deletes the
object and the bucket.

The crate has two clients. `Storage` reads and writes objects over HTTP, while
`StorageControl` creates, lists and deletes buckets and objects over gRPC, so
gcs-mimic must also serve its gRPC listener.

## Prerequisites

- Rust 1.99.1 (the repository's `rust-toolchain.toml` selects it through rustup)
- Docker, or a gcs-mimic binary

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 -p 127.0.0.1:4444:4444 \
  -e GCS_MIMIC_SCHEME=http -e GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444 \
  ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
./run.sh
```

`run.sh` reads `GCS_MIMIC_ENDPOINT` (default `http://localhost:4443`) and
`GCS_MIMIC_GRPC_ENDPOINT` (default `http://localhost:4444`), refuses anything
but loopback `http://` origins, clears ambient Google credential and proxy
variables, and runs `cargo run --locked`, so only the committed `Cargo.lock` is
used.

## Client setup

```rust
let storage = Storage::builder()
    .with_endpoint("http://localhost:4443")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
let control = StorageControl::builder()
    .with_endpoint("http://localhost:4444")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
```

Bucket names passed to both clients take the form `projects/_/buckets/NAME`.
