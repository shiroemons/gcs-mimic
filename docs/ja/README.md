# gcs-mimic

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

[![Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/shiroemons/gcs-mimic)](https://github.com/shiroemons/gcs-mimic/releases/latest)
[![Docker Hub](https://img.shields.io/docker/v/shiroemons/gcs-mimic?sort=semver&label=docker%20hub)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#ライセンス)

gcs-mimicは、ローカル開発とCIで使うGoogle Cloud Storageエミュレーターです。
Google Cloudのアカウントなしで、公式のCloud Storage SDKをそのまま使えます。

- **SDKでそのまま動く**: Go・Ruby・Node.js・Python・PHPが[SDKスコアカード](#sdkスコアカード)の全ケースを通過します。
- **旧設定を読み替える**: 旧フラグや`FAKE_GCS_*`の多くを受け付けます。保存データの変換やクライアントアダプターには対応範囲があります（[移行](#fake-gcs-serverから移行する)を参照）。
- **コンテナ1つ**: ディレクトリから初期ファイルを読み込み、ボリュームにデータを残せます。

[English](../../README.md) ·
[実装状態の原本](../../compatibility/features.json)

## まず動かす

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.7.0
curl --retry 5 --retry-all-errors -H 'Content-Type: application/json' \
  -d '{"name":"demo-bucket"}' 'http://localhost:4443/storage/v1/b?project=demo-project'
curl --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

最後のコマンドが`hello`を表示します。次は[SDKから接続する](#sdkから接続する)に進んでください。

- 既定では認証なしで受け付けるため、ポートは`127.0.0.1`にだけ公開します。
- コンテナを消してもデータを残すには、`-v gcs-mimic-data:/var/lib/gcs-mimic`を付けます。
- Docker Hubにも同じイメージがあります: `shiroemons/gcs-mimic:0.7.0`
- `GCS_MIMIC_SCHEME=http`を外すと、組み込みの開発用証明書でHTTPSになります。[ローカルHTTPS（英語）](../en/local-https.md)を参照してください。

## Docker Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.7.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://gcs:4443
    volumes:
      - ./data:/data:ro # optional: files to preload
      - gcs-data:/var/lib/gcs-mimic
  app:
    build: .
    environment:
      # Go and Python read this. For Node.js, remove it and use apiEndpoint.
      STORAGE_EMULATOR_HOST: http://gcs:4443
    depends_on:
      gcs:
        condition: service_healthy
volumes:
  gcs-data:
```

`docker compose up -d --wait`は、エミュレーターがhealthyになると戻ります。

`http://gcs:4443`の`gcs`はComposeのサービス名です。
サービス名を変える場合は、`GCS_MIMIC_API_BASE_URL`とアプリ側の設定の両方で`gcs`を置き換えてください。

`GCS_MIMIC_API_BASE_URL`は、resumableアップロードのURLなど、エミュレーターが返すリンクに入るアドレスです。
アプリが使うアドレスを指定します。
するとホスト上のプログラムは、返ってきたリンクの名前を解決できません。ホストからしか接続しないなら指定しないでください。

## 初期ファイルを読み込む

`/data`にディレクトリをマウントします。fake-gcs-serverと同じく、最上位のディレクトリがバケットに、ファイルがオブジェクトになります。

```text
data/
  sample-bucket/
    some_file.txt
  empty-bucket/
```

ファイルは起動のたびに反映され、同じ名前ではAPIでの変更より優先されます。ほかのオブジェクトは残ります。
[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。

## SDKから接続する

例はホストから接続する場合です。Composeのサービスからは、エミュレーターのサービス名を使って`http://サービス名:4443`（例: `http://gcs:4443`）に接続します。
クライアントは匿名で接続し、Googleには接続しません。
検証したバージョン、gRPC、署名付きURL、strict認証は[SDKの設定](sdk-setup.md)を参照してください。

**Go**: `STORAGE_EMULATOR_HOST=localhost:4443`を設定すると、通常のクライアントが認証を省きます。fake-gcs-serverでの設定がそのまま使えます。

```go
client, err := storage.NewClient(ctx)
```

**Ruby**: バケットの作成・一覧にはプロジェクトの指定が必要です。

```ruby
require "google/cloud/storage"
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
storage.service.project = "local-project"
```

**Node.js**: `apiEndpoint`を使い、`STORAGE_EMULATOR_HOST`は設定しません。このSDKはこの変数をパスとして読みます。

```js
import {Storage} from '@google-cloud/storage';
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443'});
```

**Python**: テストでは`STORAGE_EMULATOR_HOST=http://localhost:4443`（スキーム付き）も設定しています。

```python
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
client = storage.Client(project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"})
```

**PHP**:

```php
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;
$storage = new StorageClient(['projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials()]);
```

**Rails Active Storage**: 開発・テスト用の項目を`config/storage.yml`に1つ足します。本番は本番用の項目のままです。
Google以外の`universe_domain`を書くと、googleauthはGoogleのOAuthに接続しません。

```yaml
gcs_emulator:
  service: GCS
  endpoint: http://localhost:4443/
  universe_domain: gcs-emulator.invalid
  # plus project, bucket and local credentials
```

項目の全体、バケットとCORS、Railsをコンテナで動かす場合、CIは[Rails Active Storage](rails-active-storage.md)を参照してください。

## fake-gcs-serverから移行する

対象のコンテナ用途ではイメージを替え、別のデータ用ボリュームを付けます。全面的な置換を保証する手順ではありません。
`command:`のフラグ、`FAKE_GCS_*`環境変数、`./data:/data`のマウントは、多くがそのまま動きます。

```diff
 gcs:
-  image: fsouza/fake-gcs-server:1.56.1
+  image: ghcr.io/shiroemons/gcs-mimic:0.7.0
   command: ["-scheme", "http", "-external-url", "http://gcs:4443"]
   volumes:
     - ./data:/data
-    - gcs-storage:/storage
+    - gcs-data:/var/lib/gcs-mimic
```

旧設定は、起動時に`GCS_MIMIC_*`の名前付きで警告されます。都合のよいときに名前を変えてください。
公開済み0.7.0の例外と違いは次のとおりです。

| fake-gcs-server | gcs-mimic |
| --- | --- |
| `/storage`のデータ | 代わりに`/var/lib/gcs-mimic`に保存し、`-filesystem-root`は無視します。古いデータは[`gcs-mimic import fake-gcs-server`（英語）](../en/slices/import-filesystem.md)で変換します。 |
| `-backend memory` | ありません。データは再起動しても残ります。まっさらにするにはコンテナを消してください。 |
| `-event.*`（Pub/Sub通知） | ありません。通知を待つテストが止まったままにならないよう、起動を止めます。 |
| `-public-host` | 受け付けます。従来どおりHostヘッダーで照合するには`GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`も必要です。 |
| `?project=`なしの`GET /storage/v1/b` | Google Cloud Storageと同じく400を返します。SDKは最初からプロジェクトを送ります。 |
| resumableアップロードの取り消し | 499を返します。Googleが取り消し成功として定めている値で、エラーではありません。 |

[移行手順](fake-gcs-server-migration.md)と[設定・環境変数の対応](fake-gcs-server-configuration.md)を参照してください。

開発中ソースには、上限付きRAM保存、ローカルPub/Sub配信、HTTP batch・method override、
XML一覧・コピー・multipart、明示的な管理・reset・reseed、strictのローカルJWT、SDKアダプターを追加しています。
ディレクトリの継続監視、削除承認、復旧とlive `sync`コマンドは統合中です。
これらにはソースからのビルドが必要で、上の0.7.0イメージには含まれません。
[対応範囲と検証表（英語）](../en/evidence/migration-integration.md)を参照してください。
GoはRust子プロセスを使い、`NoListener`はUnix HTTPです。
`fake-gcs-server`実行ファイル名を固定したラッパーは、イメージの変更だけでは移行できず、
明示的なアダプターが必要です。

## よく使う設定

| 設定 | 用途 |
| --- | --- |
| `GCS_MIMIC_SCHEME` | `https`（既定）、`http`、`both` |
| `GCS_MIMIC_API_BASE_URL` | エミュレーターが返すリンクに入るアドレス |
| `GCS_MIMIC_GRPC_LISTEN` | 任意のgRPC待受。例: `0.0.0.0:4444` |
| `GCS_MIMIC_CORS_FALLBACK` | `allow-all`で、ブラウザーがどのオリジンからでも公開URLを読める |
| `GCS_MIMIC_AUTH_MODE` | `strict`でローカルのbearer tokenを検査。[strict認証（英語）](../en/getting-started.md#strict-local-authentication) |
| `GCS_MIMIC_LOG_LEVEL` | `debug`・`info`（既定）・`warn`・`error` |

`docker run --rm ghcr.io/shiroemons/gcs-mimic:0.7.0 --help`ですべての設定を確認できます。

## 機能の概要

✅は利用可能、◐は一部のみ、❌は利用不可です。行ごとに1つの操作を比べるもので、Google API全体との互換を示すものではありません。

| 操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| 複数バケットの作成・一覧・削除 | ✅ | ✅ | ✅ |
| 現行オブジェクトの保存・取得・削除 | ✅ | ✅ | ✅ |
| 再開可能アップロードの開始・分割送信・確定 | ✅ | ✅ | ✅ |
| 現行オブジェクト本体のコピー | ✅ | ✅ | ✅ |
| 複数回の呼び出しで継続するrewrite | ✅ | ❌ | ❌ |
| オブジェクトの世代履歴を保持 | ✅ | ◐ メモリ方式 | ❌ |
| APIで保存した本体を再起動後も保持 | ✅ | ✅ ファイル方式 | ✅ ディスク方式 |
| APIの保存内容をバケット・ファイルとして見る | ✅ 書き出し | ✅ ファイル方式 | ✅ ディスク方式 |
| バケット・オブジェクトの初期ファイル読込 | ✅ 毎回の起動時 | ✅ 起動・reseed | ❌ |
| JSON APIの既定CORS | ✅ | ✅ | ❌ |
| バケットごとのXML CORSを強制 | ✅ | ❌ | ❌ |
| 署名URLでのアップロード・ダウンロード | ✅ | ✅ | ✅ |
| V4 RSA署名と期限を検証 | ✅ | ❌ | ❌ |
| GoクライアントでgRPC Storage v2 | ✅ | ❌ | ❌ |
| 比較対象の旧Storage v1 gRPC RPC | ✅ | ◐ 最初のアップロードメッセージのみ | ❌ |
| 公開Dockerイメージ | ✅ GHCR・Docker Hub | ✅ | ✅ |

比較対象はfake-gcs-server`v1.56.1`とgcp-storage-emulator`v2026.07.19`です。
書き出しの行は任意の[同期ディレクトリ](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)を使い、gRPCの行は一部のRPCが対象です。
[機能比較](feature-comparison.md)と[詳細な機能表](feature-matrix.md)を参照してください。

### SDKスコアカード

公式SDKごとに、同じ操作を3つのエミュレーターで実行します（[詳細（英語）](../../sdk-scorecard/README.md)）。

| SDK | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Go | 57/57 | 24/57 | 20/57 |
| Ruby | 51/51 | 24/51 | 22/51 |
| Node.js | 51/51 | 25/51 | 22/51 |
| PHP | 51/51 | 25/51 | 20/51 |
| Python | 51/51 | 19/51 | 24/51 |
| スコア（/100） | 100 | 44.9 | 41.5 |

2026-10-29に、コミット`e7fb384`で記録しました（[結果](../../sdk-scorecard/results/2026-10-29-5/summary.md)）。
ケースの通過は通常のSDKの操作が動くことを示すもので、APIの完成を意味しません。

## 次に読む

- [Dockerでの使い方](container-usage.md)、[SDKの設定](sdk-setup.md)、[Rails Active Storage](rails-active-storage.md)
- [fake-gcs-serverからの移行](fake-gcs-server-migration.md)と[設定・環境変数の対応](fake-gcs-server-configuration.md)
- [機能比較](feature-comparison.md)と[詳細な機能表](feature-matrix.md)
- [ファイル保存の仕組み](storage-backends.md)と[ロードマップ](roadmap.md)
- [開発とpush前の確認](development.md)と[バージョンと公開の方針](release-policy.md)
- 英語ガイド: [ソースからのビルドとAPIの手順](../en/getting-started.md)、[ローカルHTTPS](../en/local-https.md)、
  [署名付きURL](../en/signed-urls.md)、[ネイティブバイナリ](../en/native-binary.md)

## ライセンス

Copyright (c) 2026 shiroemons

[MIT](../../LICENSE-MIT)または[Apache-2.0](../../LICENSE-APACHE)を選べます。
第三者の成果物には、それぞれのライセンスが適用されます。
本プロジェクトはGoogleの公式製品ではありません。
