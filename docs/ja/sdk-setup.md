# SDKの設定

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

[English](../en/sdk-setup.md)

- ホストからは`http://localhost:4443`に接続します（`GCS_MIMIC_SCHEME=http`のとき）。
- 別のComposeサービスからは`http://サービス名:4443`に接続します。サービス名はエミュレーターのもので、READMEの例では`gcs`です。エミュレーターの`GCS_MIMIC_API_BASE_URL`にも同じアドレスを指定します。
- プロジェクトIDは任意のローカル名でかまいません。クライアントは匿名で接続し、Googleやメタデータサーバーには接続しません。

`STORAGE_EMULATOR_HOST`の意味はSDKごとに異なります。

| SDK | エミュレーターの指定方法 | `STORAGE_EMULATOR_HOST`の値 |
| --- | --- | --- |
| Go | `STORAGE_EMULATOR_HOST`または`option.WithEndpoint` | `host:port`または完全なURL |
| Ruby | `Storage.anonymous(endpoint:)` | 使わない |
| Rails | `endpoint`と`universe_domain`を書いた`storage.yml`の項目 | 使わない |
| Node.js | `apiEndpoint` | 設定しない |
| Python | `api_endpoint`。テストでは変数も設定 | `http://`付きの完全なURL |
| PHP | `apiEndpoint` | 使わない |

バージョンは、リポジトリのSDKテストと[SDKスコアカード（英語）](../../sdk-scorecard/README.md)で固定しているものです。
ほかのバージョンは検証していません。

## Go

`cloud.google.com/go/storage` v1.70.0（[テスト（英語）](../../sdk-tests/go/README.md)）。
`STORAGE_EMULATOR_HOST=localhost:4443`があれば、通常の`storage.NewClient(ctx)`が認証を省いてエミュレーターに接続します。
fake-gcs-serverでの設定がそのまま使えます。コードで指定する場合は次のとおりです。

```go
import (
    "cloud.google.com/go/storage"
    "google.golang.org/api/option"
)

client, err := storage.NewClient(ctx,
    option.WithEndpoint("http://localhost:4443/storage/v1/"),
    option.WithoutAuthentication())
```

gRPCでは、エミュレーターを`GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444`で起動してポートを公開し、
`STORAGE_EMULATOR_HOST_GRPC=localhost:4444`を設定して`storage.NewGRPCClient(ctx, option.WithoutAuthentication())`を使います。

## Ruby

`google-cloud-storage` 1.62.1（[テスト（英語）](../../sdk-tests/ruby/README.md)）:

```ruby
require "google/cloud/storage"

storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
# An anonymous client has no project; set one to create and list buckets.
storage.service.project = "local-project"
bucket = storage.create_bucket "demo-bucket"
```

`endpoint`の末尾の`/`は必要です。

## Rails Active Storage

`config/storage.yml`に項目を1つ足すだけで、開発・テストではエミュレーター、本番ではGoogle Cloud Storageを使えます。
その項目でGoogle以外の`universe_domain`を設定するため、googleauthはGoogleのOAuthに接続しません。
項目の書き方、バケットとCORS、Railsをコンテナで動かす場合、CI、確認済みの範囲は[Rails Active Storage](rails-active-storage.md)を参照してください。

## Node.js

Node.js 24以降で`@google-cloud/storage` 8.4.0（[テスト（英語）](../../sdk-tests/node/README.md)）:

```js
import {Storage} from '@google-cloud/storage';

// Leave STORAGE_EMULATOR_HOST unset: this SDK reads it as a JSON API path.
const storage = new Storage({
  projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443',
});
```

`apiEndpoint`を指定すると、SDKは認証情報なしでリクエストを送ります。

## Python

`google-cloud-storage` 3.17.1（[テスト（英語）](../../sdk-tests/python/README.md)）。
SDKがApplication Default Credentialsを探さないよう、`api_endpoint`と匿名の認証情報を渡します。
テストでは`STORAGE_EMULATOR_HOST=http://localhost:4443`（スキーム付き）も設定しています。

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

`google/cloud-storage` 2.6.1（[テスト（英語）](../../sdk-tests/php/README.md)）:

```php
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;

$storage = new StorageClient([
    'projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials(),
]);
```

## 署名付きURLと認証

- 署名付きURLは、SDKによって追加のオプションが必要です。たとえばPHPでは`bucketBoundHostname`が必要です。[署名付きURL（英語）](../en/signed-urls.md)と各SDKのテストのREADMEを参照してください。
- これらの例は、すべてのリクエストを受け付ける既定のpermissiveプロファイルを使います。ローカルのbearer tokenと権限を検査するには[strict認証（英語）](../en/getting-started.md#strict-local-authentication)を参照してください。
- HTTPSを使う場合は、[ローカルHTTPS（英語）](../en/local-https.md)の手順で開発用CAを信頼させます。
