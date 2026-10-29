# gcs-mimic

A local Google Cloud Storage emulator for development and CI, written in Rust.
Point your application at it to create buckets and upload, download or delete
objects without a Google Cloud account.

This repository mirrors the images published to
`ghcr.io/shiroemons/gcs-mimic`; both registries serve the same digests for
`linux/amd64` and `linux/arm64`. Pulling needs no login.

## Quick start

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http shiroemons/gcs-mimic:latest
curl --fail \
  -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl --fail 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

The last command prints `hello`. Without `GCS_MIMIC_SCHEME=http` the server
uses HTTPS with a built-in public development certificate. The default profile
accepts unauthenticated requests, so publish the port on `127.0.0.1` only. Add
`-v gcs-mimic-data:/var/lib/gcs-mimic` to keep data after the container is
removed.

## Tags

- `X.Y.Z`: an exact release. Exact tags never move; pin one in CI.
- `X.Y`: the highest patch release of that minor version.
- `latest`: the highest stable release. It can move to a new minor version with
  breaking changes, so do not use it in CI.

Find versions, release notes and index digests on the
[GitHub Releases page](https://github.com/shiroemons/gcs-mimic/releases).

## Documentation

- [README and SDK connection examples](https://github.com/shiroemons/gcs-mimic#readme)
- [Docker image guide](https://github.com/shiroemons/gcs-mimic/blob/main/docs/en/container-usage.md)
- [Migrating from fake-gcs-server](https://github.com/shiroemons/gcs-mimic/blob/main/docs/en/fake-gcs-server-migration.md)
- [Feature matrix](https://github.com/shiroemons/gcs-mimic/blob/main/docs/en/feature-matrix.md)
- [日本語ドキュメント](https://github.com/shiroemons/gcs-mimic/blob/main/docs/ja/README.md)

gcs-mimic is an independent project and is not an official Google product. It is
dual-licensed under
[MIT](https://github.com/shiroemons/gcs-mimic/blob/main/LICENSE-MIT) or
[Apache-2.0](https://github.com/shiroemons/gcs-mimic/blob/main/LICENSE-APACHE).
