# Java example

A minimal program that uses the official
[`google-cloud-storage`](https://cloud.google.com/java/docs/reference/google-cloud-storage/latest/overview)
client (2.76.0, JSON API over HTTP) against a local gcs-mimic. It creates a
bucket, uploads `hello.txt`, lists the bucket, downloads and prints the object,
then deletes the object and the bucket (also when a step fails).

## Prerequisites

- Java 17 or newer (verified with Temurin 27.0.1.0.2+1)
- Maven 3.10.0
- A running gcs-mimic with plain HTTP:

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
bash run.sh
```

`run.sh` reads `GCS_MIMIC_ENDPOINT` (default `http://localhost:4443`) and
refuses anything but a loopback HTTP origin. It clears ambient Google
credential, project and proxy variables, fails if the resolved dependency graph
differs from `dependencies.lock`, then compiles and runs `example.Main`.

Expected output (the bucket suffix is random):

```text
created bucket example-java-d7f576a6
uploaded hello.txt
listed hello.txt (21 bytes)
downloaded: Hello from gcs-mimic!
deleted hello.txt and bucket example-java-d7f576a6
```

## Client setup

```java
Storage storage = HttpStorageOptions.newBuilder()
    .setHost(endpoint) // e.g. "http://localhost:4443"
    .setProjectId("local-project")
    .setCredentials(NoCredentials.getInstance())
    .build()
    .getService();
```

`NoCredentials` sends anonymous requests, so the client never looks up
Application Default Credentials or the metadata server.

## Updating the dependency lock

Maven has no native lockfile. After an intentional version change in
`pom.xml`, regenerate the recorded graph and review the diff:

```sh
mvn --batch-mode --no-transfer-progress dependency:tree \
  -DoutputType=text -DoutputFile=dependencies.lock
```
