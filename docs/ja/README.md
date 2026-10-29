# gcs-mimic

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

gcs-mimicは、ローカル開発とCIで使うGoogle Cloud Storageエミュレーターです。
Google Cloudのアカウントなしで、バケット作成やファイルの保存・取得・削除を試せます。
`docker run`ひとつで起動し、ディレクトリをマウントすれば初期ファイルを読み込み、
ボリュームを付ければデータを残せます。

[English](../../README.md) ·
[実装状態の原本](../../compatibility/features.json)

## まず動かす

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 ghcr.io/shiroemons/gcs-mimic:0.3.0
curl -k 'https://localhost:4443/storage/v1/b?project=local-project'
```

既定ではHTTPSで待ち受け、組み込みの公開された開発用証明書を使うため、`curl -k`で検証を省きます。
2つ目のコマンドは`{"kind":"storage#buckets"}`を表示します。
Google Cloud Storageと同じく、バケット一覧には`?project=`が必要です。省略するとエラーメッセージで案内します。

SDKから使う場合はHTTPのほうが手軽です。コンテナをHTTPで起動し直し、
バケットを作って`hello`を保存し、読み出します。

```sh
docker rm -f gcs-mimic
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.3.0
curl --fail \
  -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl --fail 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

最後のコマンドで`hello`が表示されます。既定のプロファイルは認証を要求しないため、
例ではポートを`127.0.0.1`だけに公開しています。zshなどで`?`が展開されないよう、URLは引用符で囲みます。
データはコンテナを削除するまで残ります。削除後も残すには`-v gcs-mimic-data:/var/lib/gcs-mimic`を追加します。

イメージは`linux/amd64`と`linux/arm64`向けに公開しています。
Docker Hubでも同じdigestのイメージを`shiroemons/gcs-mimic:0.3.0`として取得できます。
[0.3.0のリリースノート（英語）](../en/releases/0.3.0.md)、
[0.1.0の機能と既知の制限（英語）](../en/releases/0.1.0.md)、
[ソースからの導入手順（英語）](../en/getting-started.md)も参照してください。

### Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.3.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
    volumes:
      - gcs-data:/var/lib/gcs-mimic
volumes:
  gcs-data:
```

`docker compose up -d --wait`はイメージのヘルスチェックが通るまで待ちます。
上のcurlはそのまま使えます。`docker compose down`で停止しても、名前付きボリュームにデータが残ります。

## 初期ファイルを読み込む

fake-gcs-serverと同じ`BUCKET/OBJECT`配置のディレクトリを`/data`にマウントします。

```text
data/
  sample-bucket/
    some_file.txt
  empty-bucket/
```

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http \
  -v "$PWD/data:/data:ro" ghcr.io/shiroemons/gcs-mimic:0.3.0
curl 'http://localhost:4443/storage/v1/b?project=local-project'
curl 'http://localhost:4443/storage/v1/b/sample-bucket/o'
```

最上位のディレクトリは`local-project`所有のバケットになり、ファイルはオブジェクトになります。
fake-gcs-serverと同じく起動のたびに反映し、同じ名前ではAPIでの変更よりファイルを優先します。
それ以外のオブジェクトは残ります。
細かい規則とfake-gcs-serverとの違いは[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。

## アプリから接続する

| 呼び出し元 | 接続先 |
| --- | --- |
| ホスト上のアプリ・ブラウザー | `http://localhost:4443` |
| 同じComposeネットワークのアプリ | `http://gcs:4443`。エミュレーター側に`GCS_MIMIC_API_BASE_URL: http://gcs:4443`を指定 |

エミュレーターは、resumableアップロードの`Location`など、APIが返すリンクに1つのAPI URLを載せます。
ホストとコンテナのポートがどちらも4443なら、既定値は`http://127.0.0.1:4443`です。
ホスト側のポートが異なる場合や、別のホスト名で接続する場合は`GCS_MIMIC_API_BASE_URL`を指定します。

次の接続方法は、リポジトリのバージョン固定したSDKテストで確認しています。
Go（`cloud.google.com/go/storage` v1.70.0、[テスト](../../sdk-tests/go/README.md)）:

```go
client, err := storage.NewClient(ctx,
    option.WithEndpoint("http://localhost:4443/storage/v1/"),
    option.WithoutAuthentication())
```

Ruby（`google-cloud-storage` 1.62.1、[テスト](../../sdk-tests/ruby/README.md)）:

```ruby
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
```

Rails Active Storageでは、通常の`service: GCS`に`endpoint: http://localhost:4443/`を指定します（末尾の`/`は必要です）。
既定の認証処理はGoogleのOAuthへ接続しようとするため、
[ローカル認証情報の手順（英語）](../en/slices/sdk-local-auth-softdelete-notes.md)と
[動作確認と制限](sdk-active-storage-scenario.md)を参照してください。
GoのgRPCクライアントは、後述のgRPC待受を`STORAGE_EMULATOR_HOST_GRPC`で指定します。
`STORAGE_EMULATOR_HOST=http://localhost:4443`に対応する他のSDKは、このプロジェクトでは検証していません。
詳しくは[Dockerでの使い方](container-usage.md#アプリから接続する)と
[SDK・認証の手順（英語）](../en/getting-started.md)を参照してください。

## よく使う設定

| 設定 | 用途 |
| --- | --- |
| `GCS_MIMIC_SCHEME` | `https`（既定）、`http`、`both` |
| `GCS_MIMIC_API_BASE_URL` | APIが返すリンクに載せる接続先（上記参照） |
| `-v NAME:/var/lib/gcs-mimic` | コンテナを作り直してもバケットとオブジェクトを残す |
| `GCS_MIMIC_GRPC_LISTEN` | 任意の平文gRPC待受。例: `0.0.0.0:4444`と`-p 127.0.0.1:4444:4444` |
| `GCS_MIMIC_AUTH_MODE` | `strict`でローカルのbearer tokenを検査。[strict認証（英語）](../en/getting-started.md#strict-local-authentication) |
| `GCS_MIMIC_LOG_LEVEL` | stderrのJSONログを`debug`・`info`（既定）・`warn`・`error`で絞り込む |

すべての設定は次のコマンドで確認できます。

```sh
docker run --rm ghcr.io/shiroemons/gcs-mimic:0.3.0 --help
```

fake-gcs-serverから移行する場合、旧フラグと既知19個の`FAKE_GCS_*`環境変数は、
対応する`GCS_MIMIC_*`設定の案内付きでエラーになります。未知の`FAKE_GCS_*`は警告です。
`gcs-mimic import fake-gcs-server`は、停止したfake-gcs-serverのファイルシステムのスナップショットから
現行オブジェクトを変換します（履歴は取り込みません）。
[移行手順](fake-gcs-server-migration.md)と[設定・環境変数の対応](fake-gcs-server-configuration.md)を参照してください。

## 機能の概要

同じ行では、3製品を同じ操作・範囲で比べます。
✅は記載範囲で利用可能、◐は一部のみ、❌は利用不可です。
Google API全体との完全互換を示すものではありません。

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

書き出しの行は、内部の保存先とは別の任意の`--sync-dir`
[ディレクトリ](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)を使い、外部での削除は取り込みません。
gRPCの行は通常バケットでの記載範囲で、台帳の個々のRPCは`PARTIAL`のままです。
制限は[機能比較](feature-comparison.md)と[gRPCの接続と制限](grpc-v2-bidi-read.md)を参照してください。

比較対象はfake-gcs-server`v1.56.1`とgcp-storage-emulator`v2026.07.19`、gcs-mimicは`0.3.0`です。
初期ファイルの読み込みは、既存バックエンドのメタデータや履歴の移行を含みません。
基準・出典・検証の範囲は[機能比較](feature-comparison.md)、
実装状態は[詳細な機能表](feature-matrix.md)を参照してください。

## 次に読む

- [Dockerでの使い方](container-usage.md): 初期ファイル、同期ディレクトリ、バケット設定ファイル、各種設定
- [機能比較](feature-comparison.md)と[詳細な機能表](feature-matrix.md)
- [fake-gcs-serverからの移行](fake-gcs-server-migration.md)と[設定・環境変数の対応](fake-gcs-server-configuration.md)
- [ファイル保存の仕組み](storage-backends.md)
- SDKの対応範囲: [UBLA](sdk-ubla-predefined-acl.md)、[ライフサイクル設定](sdk-lifecycle-config.md)、
  [move](sdk-object-move.md)、[Rails Active Storage](sdk-active-storage-scenario.md)
- [ロードマップ](roadmap.md)と[並列開発の計画](delivery-plan.md)
- [バージョンと公開の方針](release-policy.md)
- [開発とpush前の確認](development.md)

HTTPS、SDK、認証の詳しい手順は[英語の利用ガイド](../en/getting-started.md)を参照してください。

## ライセンス

Copyright (c) 2026 shiroemons

[MIT](../../LICENSE-MIT)または[Apache-2.0](../../LICENSE-APACHE)を選べます。
第三者の成果物には、それぞれのライセンスが適用されます。
本プロジェクトはGoogleの公式製品ではありません。
