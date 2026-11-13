# Rust example

Creates a bucket, uploads, lists, downloads and deletes an object with `google-cloud-storage` 1.22.0. Requires Rust 1.100.0 (selected by the repository's `rust-toolchain.toml`).

## Start gcs-mimic

The crate's `StorageControl` client speaks gRPC, so also start the gRPC listener:

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 -p 127.0.0.1:4444:4444 \
  -e GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444 \
  ghcr.io/shiroemons/gcs-mimic:0.19.1
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` and `GCSM_GRPC_ENDPOINT` to use other addresses (defaults `http://localhost:4443` and `http://localhost:4444`).

## Client setup

`Storage` reads and writes objects over HTTP; `StorageControl` manages buckets over gRPC. The retry policy makes a client give up after 30 seconds instead of the default 5 minutes when the emulator is not running:

```rust
let storage = Storage::builder()
    .with_endpoint("http://localhost:4443")
    .with_credentials(anonymous::Builder::new().build())
    .with_retry_policy(RetryableErrors.with_time_limit(Duration::from_secs(30)))
    .build()
    .await?;
let control = StorageControl::builder()
    .with_endpoint("http://localhost:4444")
    .with_credentials(anonymous::Builder::new().build())
    .with_retry_policy(RetryableErrors.with_time_limit(Duration::from_secs(30)))
    .build()
    .await?;
```
