# Go example

Creates a bucket, uploads, lists, downloads and deletes an object with `cloud.google.com/go/storage` v1.70.0. Requires Go 1.27 (`go.mod` pins the `go1.27.4` toolchain).

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.21.0
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

`STORAGE_EMULATOR_HOST` sends every request to the emulator without authentication. Set it in the environment, or in code as the example does:

```go
os.Setenv("STORAGE_EMULATOR_HOST", "http://localhost:4443")
client, err := storage.NewClient(ctx)
```
