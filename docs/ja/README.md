# gcs-mimic

<!-- markdownlint-configure-file {"MD033": {"allowed_elements": ["p", "img", "details", "summary"]}} -->

<p align="center"><img src="../assets/micket.png" alt="gcs-mimicのマスコット、ミケット" width="120"></p>

<p align="center">マスコットはミケットです。mimicの「mi」とbucketの「cket」から名付けました。</p>

<p align="center">gcs-mimicは独立したプロジェクトで、Googleの公式製品ではありません。</p>

[![Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/shiroemons/gcs-mimic)](https://github.com/shiroemons/gcs-mimic/releases/latest)
[![Docker Hub](https://img.shields.io/docker/v/shiroemons/gcs-mimic?sort=semver&label=docker%20hub)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![Image size](https://img.shields.io/docker/image-size/shiroemons/gcs-mimic?sort=semver&arch=amd64)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#ライセンス)

[English](../../README.md) ·
[実装状態の原本](../../compatibility/features.json)

**小さなコンテナ。公式SDK。Google Cloudアカウントは不要。**

ローカル開発とCIのための、Rust製Google Cloud Storageエミュレーターです。

## まず動かす

```sh
# エミュレーターを起動する
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 ghcr.io/shiroemons/gcs-mimic:latest

# 応答を確かめる。組み込みの開発用証明書を使うので-kを付ける
curl -k --retry 5 --retry-all-errors 'https://localhost:4443/storage/v1/b?project=demo-project'
```

空のバケット一覧が返ります。

- 既定では認証なしで受け付けるため、ポートは`127.0.0.1`にだけ公開します。
- コンテナを消してもデータを残すには、`-v gcs-mimic-data:/var/lib/gcs-mimic`を付けます。
- Docker Hubにも同じイメージがあります: `shiroemons/gcs-mimic:latest`
- `latest`は最新のリリースを指し、破壊的変更を含む新しいマイナーバージョンに進むことがあります。CIでは[リリース一覧](https://github.com/shiroemons/gcs-mimic/releases)から正確なバージョンを選んで固定してください。
- `-k`を使わず証明書を信頼するには、[ローカルHTTPS（英語）](../en/local-https.md)を参照してください。

### HTTPで使う

SDKは証明書を信頼しなくても使えるHTTPで接続します。以降の例もHTTPです。`--scheme http`を付けて起動し直します。

```sh
docker rm -f gcs-mimic
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:latest --scheme http

# バケットを作る
curl --retry 5 --retry-all-errors \
  -H 'Content-Type: application/json' \
  -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'

# オブジェクトをアップロードする
curl --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'

# 読み戻す
curl 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

最後のコマンドが`hello`を表示します。

### よく使うオプション

イメージ名の後ろに付けます。ハイフンは1つでも2つでも、`--name=value`の形でも指定できます。

| オプション | 内容 | Composeの`environment:`では |
| --- | --- | --- |
| `--scheme http` | `https`（既定）、`http`、`both` | `GCS_MIMIC_SCHEME: http` |
| `--port 9000` | 待ち受けるポート。同じポートを`-p 127.0.0.1:9000:9000`で公開します | `GCS_MIMIC_LISTEN: 0.0.0.0:9000` |
| `--port-http 8000` | `--scheme both`のときのHTTPポート。このポートも公開します | `GCS_MIMIC_LISTEN_HTTP: 0.0.0.0:8000` |
| `--backend memory` | すべてRAMに保持します。オブジェクト本体は既定で、使えるメモリの約半分（64 MiB〜1 GiB）までです | `GCS_MIMIC_BACKEND: memory` |
| `--log-level debug` | `debug`、`info`、`warn`、`error` | `GCS_MIMIC_LOG_LEVEL: debug` |

ディレクトリを読み込むには`-v "$PWD/data:/data:ro"`を付けます。最上位のディレクトリがバケットに、ファイルがオブジェクトになります。すべてのオプションは`docker run --rm ghcr.io/shiroemons/gcs-mimic:latest --help`で確認できます。

## SDKから接続する

例は上のHTTPサーバーにホストから接続する場合です。Composeのサービスからは、エミュレーターのサービス名を使って`http://サービス名:4443`（例: `http://gcs:4443`）に接続します。
クライアントは匿名で接続し、Googleには接続しません。
検証したバージョン、gRPC、署名付きURL、strict認証は[SDKの設定](sdk-setup.md)を、下の各クライアントの実行可能なプログラムは[examples/（英語）](../../examples/README.md)を参照してください。

<details>
<summary>Go</summary>

`STORAGE_EMULATOR_HOST=localhost:4443`を設定すると、通常のクライアントが認証を省きます。

```go
client, err := storage.NewClient(ctx)
```

</details>

<details>
<summary>Ruby</summary>

バケットの作成・一覧にはプロジェクトの指定が必要です。

```ruby
require "google/cloud/storage"
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
storage.service.project = "local-project"
```

</details>

<details>
<summary>Node.js</summary>

`apiEndpoint`を使い、`STORAGE_EMULATOR_HOST`は設定しません。このSDKはこの変数をパスとして読みます。

```js
import {Storage} from '@google-cloud/storage';
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443'});
```

</details>

<details>
<summary>Python</summary>

テストでは`STORAGE_EMULATOR_HOST=http://localhost:4443`（スキーム付き）も設定しています。

```python
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
client = storage.Client(project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"})
```

</details>

<details>
<summary>PHP</summary>

```php
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;
$storage = new StorageClient(['projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials()]);
```

</details>

<details>
<summary>Java</summary>

```java
Storage storage = HttpStorageOptions.newBuilder()
    .setHost("http://localhost:4443")
    .setProjectId("local-project")
    .setCredentials(NoCredentials.getInstance())
    .build()
    .getService();
```

</details>

<details>
<summary>.NET</summary>

```csharp
var storage = new StorageClientBuilder
{
    BaseUri = "http://localhost:4443/storage/v1/",
    UnauthenticatedAccess = true,
}.Build();
```

</details>

<details>
<summary>Rust</summary>

`Storage`は4443のHTTPでオブジェクトを読み書きし、`StorageControl`は4444のgRPCでバケットを管理します。`GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444`を設定し、そのポートも公開してください。

```rust
let storage = Storage::builder()
    .with_endpoint("http://localhost:4443")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
let control = StorageControl::builder()
    .with_endpoint("http://localhost:4444")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
```

</details>

<details>
<summary>gcloud</summary>

使い捨ての`CLOUDSDK_CONFIG`を指定すると、普段の設定に影響しません。

```sh
export CLOUDSDK_CONFIG="$(mktemp -d)"
export CLOUDSDK_CORE_PROJECT=local-project
export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE=http://localhost:4443/
export CLOUDSDK_AUTH_DISABLE_CREDENTIALS=True
```

</details>

<details>
<summary>Rails Active Storage</summary>

開発・テスト用の項目を`config/storage.yml`に1つ足します。本番は本番用の項目のままです。
Google以外の`universe_domain`を書くと、googleauthはGoogleのOAuthに接続しません。

```yaml
gcs_emulator:
  service: GCS
  endpoint: http://localhost:4443/
  universe_domain: gcs-emulator.invalid
  # plus project, bucket and local credentials
```

項目の全体、バケットとCORS、Railsをコンテナで動かす場合、組み込みのテスト署名者を使うstrict、CIは[Rails Active Storage](rails-active-storage.md)を参照してください。

</details>

## なぜgcs-mimicか

- **小さい。** 空のベースイメージに静的バイナリが1つだけです。シェルもパッケージマネージャーも、ランタイムのインストールもいりません。
- **SDKがそのまま話せる。** Go・Ruby・Node.js・PHP・Python・Java・.NET・Rustが[SDKスコアカード](#sdkスコアカード)の全ケースを通過します。クライアントの接続先を向けるだけで、コードを実行できます。
- **バケットを思いどおりに。** 起動時に好きなだけ作れて、バケットごとにCORSルールや[公開・非公開](#バケットとcorsと公開設定)を設定できます。
- **ちゃんと「ダメ」と言える。** strictでは非公開バケットを非公開のまま保ち、不正な署名付きURLを拒否します。権限のバグは本番ではなく手元で見つかります。
- **データを丁寧に扱う。** 本文は64 KiBずつストリーミングするので、オブジェクトのサイズでメモリは増えません。データと未完了のresumableアップロードは再起動後も残ります。

数字は[比べてみる](#比べてみる)にあります。

## Docker Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.17.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
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
`http://gcs:4443`の`gcs`はComposeのサービス名です。サービス名を変える場合は、ここも置き換えてください。

resumableアップロードのURLなど、エミュレーターが返すリンクには各リクエストの接続先アドレスが入るので、アプリとホスト上のプログラムが1台のサーバーを共有できます。
設定の一覧は[Dockerでの使い方](container-usage.md#よく使う設定)を参照してください。

任意の`./data`のマウントは初期ファイルの読み込みに使います。最上位のディレクトリがバケットに、ファイルがオブジェクトになります。
ファイルは起動のたびに反映され、同じ名前ではAPIでの変更より優先されます。
[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。

## バケットとCORSと公開設定

すべてのバケットを1つの設定に書けて、バケットごとにCORSルールを持たせられます。上の`gcs`サービスに追加します。

```yaml
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_BUCKETS: |
        {"version": 1, "buckets": [
          {"name": "public-assets",
           "iamPolicy": {"bindings": [{"role": "roles/storage.objectViewer", "members": ["allUsers"]}]},
           "cors": [{"origin": ["http://localhost:3000"], "method": ["GET", "HEAD", "PUT"],
                     "responseHeader": ["Content-Type"], "maxAgeSeconds": 3600}]},
          {"name": "private-uploads"}
        ]}
```

起動時に2つのバケットができ、`http://localhost:3000`のページには`public-assets`のCORSヘッダーを返します。アプリはこれまでどおり動きます。
既定のモードはすべてのリクエストを許可するので、この時点ではどちらのバケットも誰でも読めます。

### 非公開バケットを非公開にする

同じ`environment`に2行追加します。

```yaml
      GCS_MIMIC_AUTH_MODE: strict
      GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444 # never publish this port
```

これで`public-assets`は`allUsers`への許可があるので誰でも読め、`private-uploads`は認証情報がないと401を返します。
2行目は信頼済みリスナーです。Composeネットワーク上のサーバー専用のポートで、認証情報なしのリクエストをアプリとして扱います。
アプリの接続先を4443から4444に変えてください。

strictに証明書・鍵・サービスアカウントの準備はいりません。
このポリシーは読み取りだけを許可するので、ブラウザーからのアップロードには署名付きURLが必要です。
貼り付けるブロックと各部分の説明は、[strictのローカル構成（英語）](../../examples/strict-local/README.md)にあります。

### 2つのモード

既定の`permissive`には門番がいません。すべてのリクエストを許可します。`strict`はリクエストごとに権限と署名を検査します。

| | `permissive`（既定） | `strict` |
| --- | --- | --- |
| 複数のバケットとバケットごとのCORS | ✅ | ✅ |
| オブジェクトを読める相手 | 誰でも | `allUsers`のバケットは誰でも。それ以外は権限のある相手だけ |
| 署名付きURL | 検査しない（`GCS_MIMIC_SIGNED_URLS=verify`で検査する） | 期限も含めて常に検査する |
| ルールのないバケットのCORS | どのオリジンでも可 | なし |
| アプリのサーバー | そのまま接続する | 信頼済みリスナーに接続する |
| 向いている用途 | 使い始めと通常のSDKテスト | 権限と署名付きURLのテスト |

まずは既定のままで始めて、これらの検査が必要なテストになったら切り替えてください。
[どの構成を使うか](container-usage.md#どの構成を使うか)も参照してください。

## 比べてみる

4つのエミュレーターは、どれも日常の基本操作に対応しています。ここの表は選ぶときの参考用です。

gcs-mimic、fake-gcs-server、gcp-storage-emulatorはCloud Storageだけをエミュレートします。[floci-gcp](https://github.com/floci-io/floci-gcp)はより広く、1つのコンテナでPub/SubやFirestoreなど多くのGoogle Cloudサービスもエミュレートします。複数のサービスを同時にテストで使うなら候補になります。

### 数字で見る

| | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| イメージ（圧縮、linux/amd64） | 8.4 MB | 25.4 MB | 48.8 MB | 67.2 MB |
| メモリ（待機時→1,000オブジェクト読み取り後） | 9.9→29.3 MiB | 33.9→57.2 MiB | 27.3→32.0 MiB | 103.6→320.0 MiB |
| ダウンロード速度、1 KiB〜100 KiBのオブジェクト | 1.2〜2.7倍 | 1倍 | 0.08〜0.29倍 | 0.62〜1.5倍 |
| ダウンロード速度、1 MiBのオブジェクト | 0.99〜1.3倍 | 1倍 | 0.09〜0.57倍 | 0.87〜1.06倍 |
| ダウンロード速度、10 MiBのオブジェクト | 0.73〜1.15倍 | 1倍 | 0.1〜0.66倍 | 0.48〜1倍 |
| アップロード速度 | 1.1〜3.5倍 | 1倍 | 0.97〜1.5倍 | 1.2〜25倍 |

2026-11-09に0.16.0、1.56.1、v2026.07.19、0.12.0をApple siliconのMac 1台のDocker Desktop上で測定しました。最新のイメージサイズは冒頭のバッジで確認できます。
各サーバーのオブジェクトは1,000個までです。floci-gcpは`hybrid`モードで動かしました。オブジェクトをメモリに持ってバックグラウンドでディスクへ書き出すため、アップロードはディスクを待たず、メモリは保存量に応じて増えます。メモリはプロセスのRSSです。
書き込みの間に読むオブジェクトが約1,000個を超えると、0.16.0は超えた分を毎秒7,500リクエスト前後で読みました。0.17.0は8接続以上で[毎秒36,000〜63,000リクエスト（英語）](../en/evidence/read-path-performance.md#reads-past-the-read-cache-and-ram-reads)です。
[測定条件と全結果（英語）](../en/evidence/product-comparison-0.16.0.md)を参照してください。

### 機能の比較

✅は利用可能、◐は一部のみ、❌は利用不可です。行ごとに1つの操作を比べるもので、Google API全体との互換を示すものではありません。

| 操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| 複数バケットの作成・一覧・削除 | ✅ | ✅ | ✅ | ✅ |
| 現行オブジェクトの保存・取得・削除 | ✅ | ✅ | ✅ | ✅ |
| 再開可能アップロードの開始・分割送信・確定 | ✅ | ✅ | ✅ | ✅ |
| 現行オブジェクト本体のコピー | ✅ | ✅ | ✅ | ✅ |
| 複数回の呼び出しで継続するrewrite | ✅ | ❌ | ❌ | ✅ |
| オブジェクトの世代履歴を保持 | ✅ | ◐ メモリ方式 | ❌ | ✅ |
| APIで保存した本体を再起動後も保持 | ✅ | ✅ ファイル方式 | ✅ ディスク方式 | ✅ hybridモード |
| APIの保存内容をバケット・ファイルとして見る | ✅ 書き出し | ✅ ファイル方式 | ✅ ディスク方式 | ❌ |
| バケット・オブジェクトの初期ファイル読込 | ✅ 起動時または明示的なreseed | ✅ 起動・reseed | ❌ | ◐ 起動時フックのスクリプト |
| JSON APIの既定CORS | ✅ | ✅ | ❌ | ❌ |
| バケットごとのXML CORSを強制 | ✅ | ❌ | ❌ | ✅ |
| 署名URLでのアップロード・ダウンロード | ✅ | ✅ | ✅ | ✅ |
| V4 RSA署名と期限を検証 | ✅ | ❌ | ❌ | ◐ 期限のみ |
| 非公開バケットを非公開に保ち、`allUsers`で公開する | ✅ strict | ❌ | ❌ | ✅ IAM enforceモード |
| GoクライアントでgRPC Storage v2 | ✅ | ❌ | ❌ | ◐ 6件中4件 |
| 比較対象の旧Storage v1 gRPC RPC | ✅ | ◐ 最初のアップロードメッセージのみ | ❌ | ❌ |
| 公開Dockerイメージ | ✅ GHCR・Docker Hub | ✅ | ✅ | ✅ Docker Hub |

比較対象はfake-gcs-server`v1.56.1`、gcp-storage-emulator`v2026.07.19`、floci-gcp`0.12.0`です。
書き出しの行は任意の[同期ディレクトリ](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)を使い、gRPCの行は一部のRPCが対象です。
[機能比較](feature-comparison.md)、[floci-gcpの確認結果（英語）](../en/evidence/floci-gcp-comparison.md)、[詳細な機能表](feature-matrix.md)を参照してください。

### SDKスコアカード

公式SDKごとに、同じ操作を4つのエミュレーターで実行します（[詳細（英語）](../../sdk-scorecard/README.md)）。

| SDK | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| Go | 57/57 | 24/57 | 20/57 | 48/57 |
| Ruby | 51/51 | 24/51 | 22/51 | 42/51 |
| Node.js | 51/51 | 25/51 | 22/51 | 44/51 |
| PHP | 51/51 | 25/51 | 20/51 | 42/51 |
| Python | 51/51 | 19/51 | 24/51 | 39/51 |
| Java | 70/70 | 35/70 | 35/70 | 52/70 |
| .NET | 70/70（1 n/a） | 31/70（1 n/a） | 31/70（1 n/a） | 51/70（1 n/a） |
| Rust | 47/47（4 n/a） | 0/47（4 n/a） | 0/47（4 n/a） | 29/47（4 n/a） |
| スコア（/100） | 100 | 39.8 | 37.7 | 77.6 |

2026-11-09に、0.16.0のソースで記録しました（[結果](../../sdk-scorecard/results/2026-11-09/summary.md)）。
ケースの通過は通常のSDKの操作が動くことを示すもので、APIの完成を意味しません。
n/aはSDKに公開APIがないケースで、そのSDKの分母から除きます。

## fake-gcs-serverから移行する

gcs-mimicは普段の使い勝手をfake-gcs-serverから受け継いでいるので、多くのコンテナ構成は数行の変更で移行できます。
イメージを替え、`/var/lib/gcs-mimic`に新しいデータ用ボリュームを付けます。
コマンドラインのフラグと`./data:/data`のマウントは多くがそのまま動きます。
0.15.0以降は`FAKE_GCS_*`環境変数を読まないので、対応するフラグか`GCS_MIMIC_*`環境変数に置き換えてください。

ただし、すべての構成でそのまま置き換えられるわけではありません。先に次の点を確認してください。

- **`/storage`ボリュームのオブジェクトは引き継がれません。** 形式が異なり、今コピーできるのは名前と中身だけです。世代とカスタムメタデータは失われます。
- **イメージにシェルはなく、rootでも動きません。** `docker exec … sh`、シェルのヘルスチェック、シェルのentrypointは動かなくなります。
- **Goのテストに`fakestorage`を組み込んでいる場合**は、APIの一部に対応した子プロセス方式に移ります。
- **`fake-gcs-server`実行ファイルを決め打ちしたTestcontainersのモジュールやラッパー**には、アダプターが必要です。
- **バケットの一覧には`?project=`が必要です。** Cloud Storageと同じ動作で、SDKは最初から送ります。`curl`では付けてください。

手順、全項目、代わりの方法は[移行ガイド](migrate-from-fake-gcs-server.md)にあります。

## 次に読む

- [Dockerでの使い方](container-usage.md)、[SDKの設定](sdk-setup.md)、[Rails Active Storage](rails-active-storage.md)
- [fake-gcs-serverからの移行](migrate-from-fake-gcs-server.md)、[詳しい移行手順](fake-gcs-server-migration.md)、[設定・環境変数の対応](fake-gcs-server-configuration.md)
- [機能比較](feature-comparison.md)と[詳細な機能表](feature-matrix.md)
- [ファイル保存の仕組み](storage-backends.md)、[設計（英語）](../en/design.md)、[ロードマップ](roadmap.md)
- [開発とpush前の確認](development.md)と[バージョンと公開の方針](release-policy.md)
- [ボトルネックの探し方](performance-investigation.md)と[性能調査の結果・採否（英語）](../en/evidence/read-optimization-summary.md)
- [0.16.0のリリースノート（英語）](../en/releases/0.16.0.md)と[すべてのリリース](https://github.com/shiroemons/gcs-mimic/releases)
- 英語ガイド: [ソースからのビルドとAPIの手順](../en/getting-started.md)、[ローカルHTTPS](../en/local-https.md)、
  [署名付きURL](../en/signed-urls.md)、[ネイティブバイナリ](../en/native-binary.md)
- [llms.txt（英語）](../../llms.txt): コーディングエージェントがgcs-mimicをプロジェクトに組み込むときの短いガイド

## ライセンス

Copyright (c) 2026 shiroemons

[MIT](../../LICENSE-MIT)または[Apache-2.0](../../LICENSE-APACHE)を選べます。
第三者の成果物には、それぞれのライセンスが適用されます。
本プロジェクトはGoogleの公式製品ではありません。
