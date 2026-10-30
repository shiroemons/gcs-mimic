# Connect an SDK

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

[日本語](../ja/sdk-setup.md)

- From the host, connect to `http://localhost:4443` (with
  `GCS_MIMIC_SCHEME=http`).
- From another Compose service, connect to `http://SERVICE:4443`, where
  `SERVICE` is the emulator's service name (`gcs` in the README), and set
  `GCS_MIMIC_API_BASE_URL` on the emulator to the same address.
- The project ID can be any local name. Clients are anonymous and never
  contact Google or the metadata server.

`STORAGE_EMULATOR_HOST` means something different in each SDK:

| SDK | Point it at the emulator with | Value of `STORAGE_EMULATOR_HOST` |
| --- | --- | --- |
| Go | `STORAGE_EMULATOR_HOST` or `option.WithEndpoint` | `host:port` or a full URL |
| Ruby | `Storage.anonymous(endpoint:)` | Not used |
| Rails | A `storage.yml` entry with `endpoint` and `universe_domain` | Not used |
| Node.js | `apiEndpoint` | Leave unset |
| Python | `api_endpoint`; the tests also set the variable | Full URL with `http://` |
| PHP | `apiEndpoint` | Not used |

The versions are those pinned by the repository's SDK tests and the
[SDK scorecard](../../sdk-scorecard/README.md); other versions are untested.

## Go

`cloud.google.com/go/storage` v1.70.0 ([tests](../../sdk-tests/go/README.md)).
With `STORAGE_EMULATOR_HOST=localhost:4443`, a plain `storage.NewClient(ctx)`
skips authentication and uses the emulator, so a fake-gcs-server setup works
unchanged. To configure it in code:

```go
import (
    "cloud.google.com/go/storage"
    "google.golang.org/api/option"
)

client, err := storage.NewClient(ctx,
    option.WithEndpoint("http://localhost:4443/storage/v1/"),
    option.WithoutAuthentication())
```

For gRPC, start the emulator with `GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444`,
publish that port, set `STORAGE_EMULATOR_HOST_GRPC=localhost:4444` and use
`storage.NewGRPCClient(ctx, option.WithoutAuthentication())`.

## Ruby

`google-cloud-storage` 1.62.1 ([tests](../../sdk-tests/ruby/README.md)):

```ruby
require "google/cloud/storage"

storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
# An anonymous client has no project; set one to create and list buckets.
storage.service.project = "local-project"
bucket = storage.create_bucket "demo-bucket"
```

Keep the trailing `/` in `endpoint`.

## Rails Active Storage

Use the emulator in development and test, and Google Cloud Storage in
production, with one extra `config/storage.yml` entry. The entry sets a
non-Google `universe_domain`, so googleauth never contacts Google OAuth. See
[Rails Active Storage](rails-active-storage.md) for the entry, bucket and CORS
setup, Rails in containers, CI and what is verified.

## Node.js

`@google-cloud/storage` 8.4.0 on Node.js 24 or later
([tests](../../sdk-tests/node/README.md)):

```js
import {Storage} from '@google-cloud/storage';

// Leave STORAGE_EMULATOR_HOST unset: this SDK reads it as a JSON API path.
const storage = new Storage({
  projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443',
});
```

With a custom `apiEndpoint`, the SDK sends requests without credentials.

## Python

`google-cloud-storage` 3.17.1 ([tests](../../sdk-tests/python/README.md)).
Pass `api_endpoint` and anonymous credentials, so that the SDK does not look
for Application Default Credentials. The tests also set
`STORAGE_EMULATOR_HOST=http://localhost:4443`, with the scheme.

```python
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage

client = storage.Client(
    project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"},
)
```

## PHP

`google/cloud-storage` 2.6.1 ([tests](../../sdk-tests/php/README.md)):

```php
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;

$storage = new StorageClient([
    'projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials(),
]);
```

## Signed URLs and authentication

- Some SDKs need extra options for signed URLs; PHP, for example, needs
  `bucketBoundHostname`. See the [signed URL guide](signed-urls.md) and each
  SDK's test README.
- These examples use the default permissive profile, which accepts any
  request. To check local bearer tokens and permissions, see
  [strict authentication](getting-started.md#strict-local-authentication).
- To use HTTPS, trust the development CA as described in
  [local HTTPS](local-https.md).
