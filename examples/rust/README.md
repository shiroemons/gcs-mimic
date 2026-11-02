# Rust example

Creates a bucket, uploads, lists, downloads and deletes an object with
`google-cloud-storage` 1.21.0. Requires Rust 1.99.1 (selected by the
repository's `rust-toolchain.toml`).

## Start gcs-mimic

The crate's `StorageControl` client speaks gRPC, so also start the gRPC
listener:

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

Set `GCS_MIMIC_ENDPOINT` and `GCS_MIMIC_GRPC_ENDPOINT` to use endpoints other
than `http://localhost:4443` and `http://localhost:4444`.

## Client setup

`Storage` reads and writes objects over HTTP; `StorageControl` manages buckets
over gRPC:

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
