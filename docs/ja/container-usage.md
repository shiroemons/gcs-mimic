# Dockerで使う

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

最短の起動方法は[READMEの「まず動かす」](README.md#まず動かす)です。
`docker run`ひとつで組み込みの開発用証明書を使うHTTPSが起動し、`-e GCS_MIMIC_SCHEME=http`でHTTPになります。
すべての設定は`docker run --rm ghcr.io/shiroemons/gcs-mimic:0.6.0 --help`で確認できます。
イメージ`ghcr.io/shiroemons/gcs-mimic:0.6.0`は`linux/amd64`と`linux/arm64`向けに公開済みで、
ログインなしでpullできます。Docker Hubでも同じdigestのイメージを`shiroemons/gcs-mimic:0.6.0`として公開しており、
以下のコマンドではどちらの名前も使えます。CIではこのバージョンを明示してください。
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
  ghcr.io/shiroemons/gcs-mimic:0.6.0
docker exec gcs-mimic gcs-mimic healthcheck
```

readinessの確認に成功したら、READMEの保存・取得コマンドを実行します。
ホストとコンテナのポートがどちらも4443なので、通知するAPI URLは`http://127.0.0.1:4443`になります。
ポートが異なる場合は[アプリから接続する](#アプリから接続する)を参照してください。
起動中で失敗した場合は、readinessのコマンドを再実行してください。
イメージのヘルスチェックは`GCS_MIMIC_SCHEME`、`GCS_MIMIC_LISTEN`と
`GCS_MIMIC_CONFIG`で選んだJSON設定に従うため、HTTPでも上書きは不要です。
ワイルドカードの待受にはループバックで接続し、`both`ではHTTP側を確認します。
HTTPSでは組み込みの開発用CAを信頼します。独自CAは`--ca-cert PATH`で指定します。
イメージのようにサーバーがPID 1で動く場合、ヘルスチェックはComposeの`command`を含むサーバーのCLI引数も読みます。
それ以外の環境では、設定を環境変数やファイルで共有するか、
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
イメージでは`http://127.0.0.1:4443`（HTTPSなら`https://`）になり、ホスト側のポートも4443のときだけホストから正しく使えます。
例えば`-p 127.0.0.1:14443:4443`なら、`GCS_MIMIC_API_BASE_URL=http://localhost:14443`を指定します。
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

このディレクトリを読み取り専用で`/data`にマウントします。

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http \
  -v "$PWD/data:/data:ro" ghcr.io/shiroemons/gcs-mimic:0.6.0
curl 'http://localhost:4443/storage/v1/b?project=local-project'
```

Composeでは、サービスの既存の`volumes`へ`- ./data:/data:ro`を追記します。
環境変数は不要です。0.2.1以前は`GCS_MIMIC_SEED_DIR=/data`が必要で、初期ファイルを一度だけ読み込んでいました。
更新するときは[0.3.0のリリースノート（英語）](../en/releases/0.3.0.md)を参照してください。

### 初期ファイルのディレクトリが使われる条件

イメージは`GCS_MIMIC_DEFAULT_SEED_DIR=/data`（`--default-seed-dir`、JSONでは`default_seed_dir`。
nativeの既定値はなし）を設定します。`seed_dir`が未指定で、ディレクトリが存在し、
データディレクトリや同期ディレクトリと重ならない場合だけ使い、それ以外は何も表示せずに無視します。
明示した`GCS_MIMIC_SEED_DIR`（`--seed-dir`、JSONでは`seed_dir`）が常に優先するため、別のパスも指定できます。

### 起動のたびに行うこと

readinessより前に、起動のたびに初期ファイルを反映します。最上位のディレクトリはバケットになり、
未作成なら作成し、既存のバケットは設定をそのまま残します。バケットは初期ファイル用のproject
（既定は`local-project`、`GCS_MIMIC_SEED_PROJECT`で変更）に属します。
既存のバケットが別のprojectのものなら起動に失敗します。空のディレクトリからは空のバケットを作ります。
初期ファイルのルート直下にある通常のファイル（`README.md`や`.gitkeep`など）は、fake-gcs-serverと同じく無視し、
`seed_skipped`の警告を出します。シンボリックリンクと不正なバケット名では、これまでどおり起動に失敗します。

バケットのディレクトリ以下の各ファイルは、相対パスを名前とするオブジェクトになります。

| 現行オブジェクト | 結果 |
| --- | --- |
| ない | 作成する |
| 初期ファイルから書き込む内容と完全に同じ（本体と初期ファイルのContent-Typeが同じで、Content-Encoding・Content-Disposition・Content-Language・Cache-Control・custom time・独自メタデータがない） | そのまま。世代も変えず、`unchanged`として数える |
| それ以外の違いがある（APIで変更したメタデータを含む） | 初期ファイルの既定メタデータで新しい現行世代に置き換える。バージョニングが有効なバケットでは以前の世代をnoncurrentとして残す |
| 違いはあるが、オブジェクトホールドや保持期間の対象 | そのままにし、警告を出して`blocked`として数える |

ホールドとストレージクラスは比較しないため、内容が同じならホールド中のオブジェクトも`unchanged`です。

初期ファイルのないオブジェクトには触れず、初期ファイルを消してもオブジェクトは削除しません。
APIで削除したオブジェクトは次回の起動時に再作成し、初期ファイルの編集も次回の起動時に反映します。
ファイル監視やreseed用のエンドポイントはないため、コンテナを再起動してください。

Content-Typeは、固定の対応表を使ってファイルの拡張子から決めます（例: `.png`は`image/png`）。
以前の既定値どおりすべて`application/octet-stream`にするには、`GCS_MIMIC_SEED_CONTENT_TYPE=octet-stream`を指定します。
この設定は[同期ディレクトリ](#オブジェクトをホストのディレクトリへ書き出す)から取り込むファイルにも適用します。
`.DS_Store`や`._*`などのmacOSのメタデータファイルは読み飛ばします。

結果はready通知の`seed`に出ます。例えば
`{"outcome":"imported","buckets":1,"objects":2,"replaced":0,"unchanged":3,"blocked":0,"skipped_os_metadata":0}`
のようになり、`buckets`と`objects`は新しく作成した件数です。初期ファイルがない場合は`{"outcome":"disabled"}`です。

### fake-gcs-serverとの違い

fake-gcs-serverと同じく、再起動すると内容が異なる初期オブジェクトは置き換わり、APIでのメタデータ変更も残りません。
意図した違いは2点だけです。fake-gcs-serverは完全に同じオブジェクトでも新しい世代を書き込みますが、
gcs-mimicはそのまま残します。また、gcs-mimicには`/_internal/reseed`のルートがありません。
fake-gcs-serverの実行用ストレージにあるxattrやサイドカーのメタデータは移行しません。
初期ファイルのディレクトリは読み込み専用の入力で、APIで保存しても`/data`へは書き戻しません。
APIで保存したオブジェクトをファイルとして見るには、別の[同期ディレクトリ](#オブジェクトをホストのディレクトリへ書き出す)を使います。
CORSやバージョニングの設定付きでバケットを作るには[バケット設定ファイル](#初期バケット設定ファイルを使う)を使います。
こちらは初期ファイルより先に動きます。既存の永続データを使う前に[保存方式の違い](storage-backends.md)を確認してください。

## オブジェクトをホストのディレクトリへ書き出す

`GCS_MIMIC_SYNC_DIR`（`--sync-dir`、JSONでは`sync_dir`）を指定すると、既存の書き込み可能な
ディレクトリへ、すべてのバケットと現行オブジェクトを`BUCKET/OBJECT`のファイルとして書き出します。
READMEのサービスへ次の項目を追加します。既存の環境変数と永続ボリュームは残してください。

```yaml
    environment:
      GCS_MIMIC_SYNC_DIR: /sync
    volumes:
      - ./sync:/sync
```

起動前に`./sync`を作り、あとは通常どおりAPIを使います。

```sh
mkdir -p sync
docker compose up -d --wait
curl --fail -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=docs%2Fhello.txt'
cat sync/demo-bucket/docs/hello.txt
```

readinessの成功後、既存のバケット（空のバケットを含む）とオブジェクトを書き出します。
以後はAPIでの作成・上書き・コピー・compose・削除が、応答の少し後にファイルへ反映されます。
応答した時点でファイルがあるとは限らないため、必要ならポーリングしてください。
保存状態の本体はSQLiteのカタログと内部ファイルで、このディレクトリは写しです。
データの永続化には、引き続き`/var/lib/gcs-mimic`のボリュームが必要です。

サーバーの外で作成・編集したファイルは、起動時に新しい世代としてhash付きで取り込みます。
ファイル監視はないため、編集を反映するには再起動します。取り込んだファイルがAPIから
見えるのは、readinessの約1秒後です。ファイルを消してもオブジェクトは削除されず、
APIでバケットを削除してもディレクトリは残ります。同じオブジェクトをAPIとファイルの両方で
変更した場合は、どちらも相手を上書きせず、ファイルはそのまま残ります。起動時の
`sync_ingested`・`sync_reconciled`ログイベントでは、こうしたファイルを`conflicting`として数えます。
`logs/2026-10-28T12:00:00.log`のように`\`や`:`を含む名前と、ホストのファイル名に
対応付けられないオブジェクト名は書き出さずに飛ばします。書き出し自体は止まらず、APIからは使えます。

ディレクトリには、1つのデータディレクトリと結び付ける`.gcs-mimic-sync.json`と
`.gcs-mimic-sync-work`が作られます。どちらも`.gitignore`に追加してください。
`docker compose down --volumes`などでデータボリュームを新しくすると、古いディレクトリは
`ForeignMarker`の診断で起動時に拒否されます。古いディレクトリは退避し、新しい空のディレクトリを
使ってください。データや初期ファイルのディレクトリと重ねることはできません。
同期にはpermissive認証が必要で（strictでは拒否します）、LinuxとmacOSで動作します。
Windowsでは拒否します。

書き出したファイルとディレクトリは、サーバーのユーザーだけが読めるモード（`0600`・`0700`）です。
Docker Desktopはbind mountの所有者を対応付けるため、macOSのホストでは読めます。
Linuxのホストでは、イメージのUID `10001`の所有になります。コンテナを自分のユーザーで実行し、
データディレクトリも同じユーザーが所有するbind mountにしてください。

```sh
mkdir -p data sync
docker run -d --name gcs-mimic --user "$(id -u):$(id -g)" \
  -p 127.0.0.1:4443:4443 \
  -v "$PWD/data:/data" -v "$PWD/sync:/sync" \
  -e GCS_MIMIC_SCHEME=http -e GCS_MIMIC_DATA_DIR=/data -e GCS_MIMIC_SYNC_DIR=/sync \
  ghcr.io/shiroemons/gcs-mimic:0.6.0
```

検証済みの範囲、復旧の挙動、未実装の作業は[同期の契約（英語）](../en/slices/directory-synchronization.md)を参照してください。

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
fake-gcs-serverのように、ルールのないバケットのXML・公開URLをどのオリジンからでも読めるようにするには、
`GCS_MIMIC_CORS_FALLBACK=allow-all`を追加します（[CORSフォールバックの仕様](../en/slices/cors-fallback.md)、英語）。

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

- [HTTPSと証明書](../en/local-https.md#local-https-with-an-explicit-development-ca)
- [SDK・認証・ブラウザー](../en/getting-started.md)
- [イメージ配布と対応環境](../en/container-distribution.md)
- [今後の導入機能](roadmap.md)
