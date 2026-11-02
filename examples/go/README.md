# Go example

Uses the official [`cloud.google.com/go/storage`](https://pkg.go.dev/cloud.google.com/go/storage)
client (v1.70.0) against a local gcs-mimic: it creates a bucket, uploads a text
object, lists the bucket, downloads the object and prints it, then deletes the
object and the bucket.

## Prerequisites

- Go 1.27 (`go.mod` pins the `go1.27.3` toolchain; with the default
  `GOTOOLCHAIN=auto` an older `go` downloads it)
- Docker, or a gcs-mimic binary

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
./run.sh
```

`run.sh` reads `GCS_MIMIC_ENDPOINT` (default `http://localhost:4443`), refuses
anything but a loopback `http://` origin, clears ambient Google credential and
proxy variables, and runs `go run .` with `-mod=readonly`, so only the committed
`go.sum` is used.

## Client setup

The Go client talks to an emulator when `STORAGE_EMULATOR_HOST` is set: every
request then goes to that host without authentication. The example sets it from
`GCS_MIMIC_ENDPOINT`:

```go
os.Setenv("STORAGE_EMULATOR_HOST", "http://localhost:4443")
client, err := storage.NewClient(ctx)
```

In your own code, set `STORAGE_EMULATOR_HOST=localhost:4443` in the environment
instead and keep `storage.NewClient(ctx)` unchanged.
