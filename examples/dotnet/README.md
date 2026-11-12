# .NET example

Creates a bucket, uploads, lists, downloads and deletes an object with the official `Google.Cloud.Storage.V1` 5.0.0 client. Requires .NET SDK 11.0.100 (`global.json`) and targets `net11.0`, the runtime that SDK installs.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.19.1
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

```csharp
var storage = new StorageClientBuilder
{
    BaseUri = "http://localhost:4443/storage/v1/",
    UnauthenticatedAccess = true,
}.Build();
```
