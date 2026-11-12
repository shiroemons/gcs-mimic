# Java example

Creates a bucket, uploads, lists, downloads and deletes an object with the official `google-cloud-storage` 2.76.0 client. Requires Java 17+ and Maven 3.10.0.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.19.0
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

```java
Storage storage = HttpStorageOptions.newBuilder()
    .setHost("http://localhost:4443")
    .setProjectId("local-project")
    .setCredentials(NoCredentials.getInstance())
    .build()
    .getService();
```
