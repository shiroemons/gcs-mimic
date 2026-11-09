# .NET example

Creates a bucket, uploads, lists, downloads and deletes an object with the official `Google.Cloud.Storage.V1` 5.0.0 client. Requires .NET SDK 10.0.403 (`global.json`).

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.15.0
```

## Run

```sh
./run.sh
```

Set `GCS_MIMIC_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

```csharp
var storage = new StorageClientBuilder
{
    BaseUri = "http://localhost:4443/storage/v1/",
    UnauthenticatedAccess = true,
}.Build();
```
