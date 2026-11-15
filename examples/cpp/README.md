# C++ example

Creates a bucket, uploads, lists, downloads and deletes an object with google-cloud-cpp 3.13.0. Needs only Docker: the SDK has no binary package, so the image builds it from checksum-verified sources with GCC 16.2.0, using the versions and steps of the [scorecard probe](../../sdk-scorecard/cpp/README.md). The first build compiles the SDK and its dependencies and takes several minutes; later builds reuse Docker's cache.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.23.0
```

## Run

```sh
./run.sh
```

The example container joins the network of the container named `gcs-mimic`, so `localhost:4443` reaches it. Set `GCSM_DOCKER_NETWORK` to run it on another Docker network, for example a Compose project's, and `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

The SDK is built for its REST transport only, which needs no gRPC listener. Insecure credentials send no `Authorization` header, so the client never contacts Google:

```cpp
namespace gc = ::google::cloud;
namespace gcs = ::google::cloud::storage;
auto client = gcs::Client(gc::Options{}
    .set<gcs::RestEndpointOption>("http://localhost:4443")
    .set<gcs::ProjectIdOption>("local-project")
    .set<gc::UnifiedCredentialsOption>(gc::MakeInsecureCredentials()));
```

When `CLOUD_STORAGE_EMULATOR_ENDPOINT` is set, the SDK sends every request there and ignores `RestEndpointOption`; the example stops if it names another address. The example also limits retries to 30 seconds instead of the default 15 minutes, so it fails quickly when the emulator is not running.
