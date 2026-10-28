# Dockerで使う

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

最短の起動方法は[READMEのCompose例](README.md#まず動かす)です。
イメージ`ghcr.io/shiroemons/gcs-mimic:0.2.1`は`linux/amd64`と`linux/arm64`向けに公開済みで、
ログインなしでpullできます。CIではこのバージョンを明示してください。
未公開のソースを試す場合は、チェックアウトでローカルイメージをビルドし、以下のコマンドのイメージ名を置き換えます。

```sh
docker build -t gcs-mimic:local .
```

[Composeの移行手順](fake-gcs-server-migration.md)では、fake-gcs-serverのサービスを公開イメージに置き換え、
空のバケットも初期化します。対象はUX-012、DESIGN-061、UX-001です。

[English](../en/container-usage.md) ·
[実装状態の原本](../../compatibility/features.json)

## docker runで起動する

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 \
  -v gcs-mimic-data:/var/lib/gcs-mimic \
  -e GCS_MIMIC_SCHEME=http \
  -e GCS_MIMIC_API_BASE_URL=http://localhost:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.2.1
docker exec gcs-mimic gcs-mimic healthcheck
```

readinessの確認に成功したら、READMEの保存・取得コマンドを実行します。
起動中で失敗した場合は、readinessのコマンドを再実行してください。
イメージのヘルスチェックは`GCS_MIMIC_SCHEME`、`GCS_MIMIC_LISTEN`と
`GCS_MIMIC_CONFIG`で選んだJSON設定に従うため、HTTPでも上書きは不要です。
ワイルドカードの待受にはループバックで接続し、`both`ではHTTP側を確認します。
HTTPSでは組み込みの開発用CAを信頼します。独自CAは`--ca-cert PATH`で指定します。
サーバーのCLI引数は別プロセスから読めません。設定を環境変数やファイルで共有するか、
`healthcheck --url URL`で接続先を明示してください。
イメージは`GCS_MIMIC_LISTEN=0.0.0.0:4443`と
`GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`を環境変数で設定します。
ポートや保存先を変える場合は、この環境変数を上書きしてください。
nativeと同じく、環境変数はJSON設定より優先します。
例えば`-e GCS_MIMIC_LISTEN=0.0.0.0:8080 -p 127.0.0.1:8080:8080`で
待受と公開ポートを変えます。
保存先は`-e GCS_MIMIC_DATA_DIR=/custom-data -v gcs-mimic-data:/custom-data`で変えます。
JSONの`listen`や`data_dir`だけでは、イメージの環境変数を上書きできません。

移行時のバケット一覧では、`legacy-emulator`はprojectを省略すると
`seed_project`を使います。nativeは400を返し、`?project=local-project`の例を示します。
明示したprojectの検証・絞り込み・認可は維持します。
対象はDESIGN-067、DESIGN-058、JSON-BUCKETS-006です。

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
移行用Composeでは`http://gcs:4443`を最初から指定し、コンテナのSDKが
resumableのLocationへ接続できるようにします。ホストやブラウザーは
`http://localhost:14443`などの公開先を使います。署名のないセッションURLの変換、
取り消し時の499、作業ツリー別のポートは移行手順を参照してください。

API URLを明示しない場合、実際にbindしたアドレスとポートから決まります。
`0.0.0.0`は`127.0.0.1`、`::`は`[::1]`に置き換え、ポート0は割り当て後のポートを使います。
ready通知の`api_base_url`で確認できます。
リポジトリのComposeファイルでは、`GCS_MIMIC_PORT`のホスト側ポート（既定4443）に合わせます。
`GCS_MIMIC_API_BASE_URL`の明示値があれば、そちらを優先します。
Dockerのホスト側ポートを0にした場合は、割り当て後の値を調べ、通知するURLを再設定してください。
サーバーはホスト側のポート割り当てやプロキシの外部アドレスを自動取得できません。

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
初期化後に足したバケットディレクトリは、再起動しても作成されません。
空ディレクトリでのバケット準備は[バケット設定ファイル](#初期バケット設定ファイルを使う)に
置き換えます。こちらは毎回、未作成のバケットを作ります。

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
