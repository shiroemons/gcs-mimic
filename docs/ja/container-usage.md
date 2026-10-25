# Dockerで使う

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

最短の起動方法は[READMEのCompose例](README.md#まず動かす)です。
イメージ`ghcr.io/shiroemons/gcs-mimic:0.1.0`は未公開で、まだpullできません。
以下も公開後、または検証済みのローカルイメージに置き換えて使います。
CIでは公開済みのバージョンを明示してください。

[English](../en/container-usage.md) ·
[実装状態の原本](../../compatibility/features.json)

## docker runで起動する

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 \
  -v gcs-mimic-data:/var/lib/gcs-mimic \
  -e GCS_MIMIC_SCHEME=http \
  -e GCS_MIMIC_API_BASE_URL=http://localhost:4443 \
  --no-healthcheck \
  ghcr.io/shiroemons/gcs-mimic:0.1.0
docker exec gcs-mimic gcs-mimic healthcheck \
  --url http://127.0.0.1:4443/_internal/ready
```

readinessの確認に成功したら、READMEの保存・取得コマンドを実行します。
起動中で失敗した場合は、readinessのコマンドを再実行してください。
このHTTP例ではイメージ既定のHTTPSヘルスチェックを無効にしています。
Compose例ではHTTP用の自動ヘルスチェックを設定しています。

`docker stop gcs-mimic`で停止し、`docker start gcs-mimic`で再開できます。
停止したコンテナを削除しても、名前付きボリュームのデータは残ります。
スキームを指定しない場合、サーバーの既定値はHTTPSです。

## アプリから接続する

| 呼び出し元 | APIエンドポイント |
| --- | --- |
| ホスト上のアプリ・ブラウザー | `http://localhost:4443` |
| 同じComposeネットワークのアプリ | `http://gcs:4443` |

コンテナ内の`localhost`は、そのコンテナ自身を指します。
APIが返すURLも使う場合は、接続元から到達できるアドレスを
`GCS_MIMIC_API_BASE_URL`へ指定します。コンテナ間なら`http://gcs:4443`が例です。
サーバーが通知するAPIエンドポイントは1つです。
公開URLや署名URLも、受け取るクライアントから到達できるアドレスで作ります。

## 初期ファイルを読み込む

fake-gcs-serverの`/data`と同じバケット・オブジェクト配置を使えます。

```text
data/
  assets/
    logo.png
  uploads/
    example.txt
```

初回起動前に、READMEのサービスへ次の項目を追加します。
既存の`environment`と`volumes`へ追記し、各セクションを置き換えないでください。

```yaml
    environment:
      GCS_MIMIC_SEED_DIR: /data
    volumes:
      - ./data:/data:ro
```

永続カタログの初期化時だけ、readinessの成功前にファイルを読み込みます。
再起動時に初期ファイルを再読込して、APIの変更を上書きすることはありません。
初期バケットの所有プロジェクトは既定で`local-project`です。
変更する場合は`GCS_MIMIC_SEED_PROJECT`を指定します。

すでにREADMEの例を起動した場合は、`docker compose down`で停止します。
次に`docker compose -p gcs-seeded up -d --wait`で別プロジェクトを起動できます。
別の新しいボリュームが作られ、元のプロジェクトのデータは残ります。

現在、初期ファイルのContent-Typeは`application/octet-stream`になります。
fake-gcs-serverの実行用ストレージにあるxattrやサイドカーのメタデータは移行しません。
初期化後の外部編集は反映されず、APIで保存しても`/data`へは書き戻しません。
既存の永続データを使う前に[保存方式の違い](storage-backends.md)を確認してください。

## バケットごとに設定する

待受アドレス、HTTP/HTTPS、保存先、通知するAPI URLはサーバー全体の設定です。
Composeの環境変数かCLIで指定します。設定名の対応は
[設定・環境変数](fake-gcs-server-configuration.md)を参照してください。

CORSなどはバケットごとにJSON APIで設定します。

```sh
curl --fail -H 'Content-Type: application/json' \
  -d '{"name":"web-assets","cors":[{
    "origin":["http://localhost:3000"],"method":["GET","HEAD"],
    "responseHeader":["Content-Type"],"maxAgeSeconds":3600
  }]}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
```

別バケットには別のルールを指定できます。既存ルールの変更には
`PATCH /storage/v1/b/BUCKET`を使います。バケットCORSはXML用で、
JSON APIは固定のCORSルールを使います。CORSを許可しても読み取り権限は増えません。

## 初期バケット設定ファイルを使う

ソースから実行する場合、同梱の`config/buckets.json`を読み取り専用で渡せます。
CORS・バージョニング・projectが異なる2バケットを作る例です。

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.buckets.yaml up -d --build --wait
curl --fail http://localhost:4443/storage/v1/b/initialized-assets
```

別のComposeには、既存のenvironmentへ`GCS_MIMIC_BUCKETS_FILE: /buckets.json`、
volumesへ`./buckets.json:/buckets.json:ro`を追記します。ファイルは起動前に作ります。

```json
{"version":1,"project":"local-project","buckets":[
  {"name":"web-assets","cors":[{"origin":["http://localhost:3000"],"method":["GET"]}]},
  {"name":"private-uploads","versioning":{"enabled":true}}
]}
```

毎回、全件を検証してからseedより先に未作成バケットだけを作り、以後はAPIで変更します。
再起動時に既存設定を上書きせず、ファイルから項目を消してもバケットを削除しません。
APIで削除したバケットは次回起動時に再作成します。既存projectが異なる場合は
所有者を保ち、stderrで警告し、readyの`project_mismatches`へ件数を出します。

選択順は`--buckets-file PATH`、`GCS_MIMIC_BUCKETS_FILE`、サーバーJSONの
`buckets_file`です。エントリの`project`、ファイルの`project`、`seed_project`
（既定`local-project`）の順に所有projectを決めます。
strictでもローカル管理として初期化し、API権限は付与しません。
IAMポリシー、ACL、lifecycle、UBLA、未知フィールドは空・無効でも拒否します。
JSON APIの固定CORSとXMLのバケット別CORSは別です。
上限はファイル1MiB、1000件、各64KiBです。
JSON-BUCKETS-002、JSON-BUCKETS-008、DESIGN-062はPARTIAL、
BEHAVIOR-BUCKET-009はIMPLEMENTEDを維持します。
詳細は[英語の契約](../en/slices/bucket-initialization.md)を参照してください。

## 詳しい手順

- [HTTPSと証明書](../en/getting-started.md#local-https-with-an-explicit-development-ca)
- [SDK・認証・ブラウザー](../en/getting-started.md)
- [イメージ配布と対応環境](../en/container-distribution.md)
- [今後の導入機能](roadmap.md)
