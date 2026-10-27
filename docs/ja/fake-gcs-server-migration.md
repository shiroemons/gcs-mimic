# fake-gcs-serverからのCompose移行

<!-- markdownlint-configure-file
{"MD013": {"tables": false, "code_blocks": false}}
-->

[English](../en/fake-gcs-server-migration.md) ·
[検証と操作の対応表](../en/slices/migrate-guide-compose.md)

対象はUX-012、DESIGN-061、UX-001です。既定のpermissive/nativeで、HTTP、
空バケットの初期化、Go SDK、署名のないブラウザー転送を確認しています。
UX-012とUX-001はPARTIAL、DESIGN-061は既存のホスト・コンテナ接続範囲で
IMPLEMENTEDです。永続データの移行やfake-gcs-serverの全面的な置き換えは含みません。

## fake-gcs-serverのComposeを置き換える

既存アプリのサービスと`STORAGE_EMULATOR_HOST=gcs:4443`はそのまま残し、
`gcs`サービスを置き換えます。よく使う5組のフラグは次のように書き換えます。
移行前のCompose全体は[英語版の例](../en/fake-gcs-server-migration.md#replace-a-fake-gcs-server-compose-service)を参照してください。
古いフラグと`FAKE_GCS_*`は別名として受け付けず、置き換え先を診断に表示します。

| 書き換え前 | 書き換え後 | 必要な理由・違い |
| --- | --- | --- |
| `-scheme http` | `GCS_MIMIC_SCHEME: http` | 省略時はHTTPSなので、既存のHTTPクライアントが接続できません。イメージのhealthcheckもこの値に従います。 |
| `-host 0.0.0.0 -port 4443` | `GCS_MIMIC_LISTEN: 0.0.0.0:4443` | ループバックだけで待ち受けると他のコンテナから接続できません。イメージの既定値を明示しています。 |
| `-public-host gcs:4443 -external-url http://gcs:4443` | `GCS_MIMIC_API_BASE_URL: http://gcs:4443` | コンテナのSDKがresumableのLocationへ接続するために必要です。nativeのpath-style読み取りにはpublic-host設定が不要です。旧来の仮想ホスト判定や公開権限は再現しません。 |
| `-backend filesystem -filesystem-root /storage` | `/var/lib/gcs-mimic`へ名前付きボリューム | 既定はSQLiteと内部blobの永続ストレージです。保存形式が違うため、移行元のボリュームは別に保管します。この手順にmemory backendの代替はありません。 |
| 空のバケットディレクトリを置く`-data /data` | `GCS_MIMIC_BUCKETS_FILE: /buckets.json`と読み取り専用マウント | 毎回、未作成のバケットを作るために必要です。seedはカタログの初期化時だけ動き、後から足したディレクトリはバケットになりません。 |

`data/assets/`と`data/exports/`の代わりに、起動前に`buckets.json`を作ります。

| 保持ポリシーの操作 | gcs-mimicでの動作 |
| --- | --- |
| Go `BucketAttrs.RetentionPolicy`、Ruby `bucket.retention_period = 3600` | JSON APIで保持設定を保存し、保持中の削除・上書きを403で拒否します。 |
| Go `LockRetentionPolicy`、Ruby `bucket.lock_retention_policy!` | metageneration条件と更新権限が必要です。ロック後の削除・短縮は400 badRequestExceptionになり、原因と延長例を返します。IAM拒否は403 forbiddenです。実バケットでは試験しません。 |

新しい設定は不要です。[保持ポリシーの対応範囲](sdk-retention-policy.md)を参照してください。

```json
{"version":1,"project":"local-project","buckets":[{"name":"assets"},{"name":"exports"}]}
```

空ディレクトリのseedマウントは外し、このファイルを渡します。
再起動時は未作成のバケットだけを追加し、既存のオブジェクトやAPIで変更した設定を保ちます。
ファイルから項目を消してもバケットは削除しません。次のバケットはファイルに追加して再起動します。
オブジェクトの初期ファイルは別の[初回seed](container-usage.md#初期ファイルを読み込む)で扱います。

`gcs`サービスを公開イメージに置き換え、アプリのサービスはそのままにします。
`buckets.json`はComposeファイルの隣に置きます（マウント元はComposeファイルからの相対パスです）。
例のファイル名は`compose.migration.yaml`です。既存アプリでは、自分のComposeファイルのサービスを書き換えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.1.3
    ports:
      - "127.0.0.1:${GCS_MIMIC_PORT:-4443}:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_LISTEN: 0.0.0.0:4443
      GCS_MIMIC_API_BASE_URL: http://gcs:4443
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - migration-data:/var/lib/gcs-mimic
      - ./buckets.json:/buckets.json:ro
volumes:
  migration-data:
```

APIの通知先は1つに固定し、リクエストのHostからは組み立てません。
`GCS_MIMIC_API_BASE_URL`を省略すると、bind先から導出したループバックのLocationを
他のコンテナが受け取り、自分自身へ接続してしまいます。コンテナのSDKには`gcs:4443`、
ホストやブラウザーには公開ポートの`localhost`を使います。
サーバーはDockerのポート割り当てを自動取得できません。
healthcheckの上書きは不要です。アプリの起動待ちは
`depends_on: {gcs: {condition: service_healthy}}`で指定できます。

```sh
export COMPOSE_FILE=compose.migration.yaml
export COMPOSE_PROJECT_NAME=assets-migration
export GCS_MIMIC_PORT=14443
docker compose up -d --wait
docker compose exec -T gcs gcs-mimic healthcheck
api="http://localhost:${GCS_MIMIC_PORT}"
curl --fail "$api/storage/v1/b/assets"
curl --fail "$api/storage/v1/b/exports"
curl --fail -H 'Content-Type: application/json' -d '{"name":"scratch-assets"}' \
  "$api/storage/v1/b?project=local-project"
curl --fail --data-binary 'hello' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=hello.txt"
test "$(curl --fail "$api/storage/v1/b/assets/o/hello.txt?alt=media")" = hello
```

作業ツリーごとにComposeプロジェクト名とホスト側ポートを変えます。
2つ目なら`COMPOSE_PROJECT_NAME=assets-migration-two`、`GCS_MIMIC_PORT=24443`が例です。
内部ポートと通知する`http://gcs:4443`は変えません。名前付きボリュームもプロジェクト別です。
同じファイル・プロジェクトを指定して`docker compose down`で停止すると、データは残ります。

## 別オリジンからアップロード・読み取りする

`http://localhost:3000`のページからJSON APIを認証情報なしで使う形です。
この範囲ではCORSの追加設定は不要です。次の生HTTPではpreflightとアップロードの
`Access-Control-Allow-Origin`が指定したOriginになり、読み取りで本文が返ることを確認します。

```sh
curl --fail -i -X OPTIONS -H 'Origin: http://localhost:3000' \
  -H 'Access-Control-Request-Method: POST' \
  -H 'Access-Control-Request-Headers: content-type' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=browser.txt"
curl --fail -i -H 'Origin: http://localhost:3000' -H 'Content-Type: text/plain' \
  --data-binary 'browser bytes' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=browser.txt"
curl --fail -i -H 'Origin: http://localhost:3000' \
  "$api/storage/v1/b/assets/o/browser.txt?alt=media"
curl --fail "$api/assets/browser.txt"
```

シナリオでは12本の並行アップロードと本文一致も確認します。
生HTTPでの確認は実ブラウザーのCORS制御の検証ではありません。
path-styleの匿名GETは既定のpermissiveで本文を返しますが、XMLを別オリジンからfetchする場合は
バケットのCORS設定が別途必要です。strictではCORSを許可しても権限は増えません。
[GoogleのCORS仕様](https://docs.cloud.google.com/storage/docs/cross-origin)も参照してください。

サーバーが作った署名なしのresumable URLをブラウザーへ渡す場合は、pathとqueryを保ち、
到達可能なオリジンへ置き換えます。シナリオはHost変更後のPUT、Content-Range、preflightを確認します。
取り消しのDELETEは[JSON resumableの仕様](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads)どおり
**499**を返し、その後のセッションアクセスは410になります。
499を基盤障害と扱わず、取り消しの成功として判定してください。

```sh
location=$(curl --fail -sS -D - -o /dev/null \
  -H 'Content-Type: application/json' -H 'X-Upload-Content-Type: text/plain' \
  -d '{}' "$api/upload/storage/v1/b/assets/o?uploadType=resumable&name=cancelled.txt" \
  | awk 'tolower($1)=="location:" {sub("\r$", "", $2); print $2}')
host_session="$api${location#http://gcs:4443}"
cancellation_status=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "$host_session")
test "$cancellation_status" = 499
```

`curl --fail`は499をエラー扱いするため、取り消しではコードを明示的に検査します。
このオリジン変更は署名なしのpermissiveセッションに限ります。
strictの署名URLは最終的に利用者がアクセスするURLで署名し、後から書き換えません。

## 移行で困ったとき

| 症状 | 原因 | 直し方 |
| --- | --- | --- |
| `-scheme`や`FAKE_GCS_SCHEME`で起動が止まる | 旧設定名は別名ではない | 古いcommandやキーを消し、`GCS_MIMIC_SCHEME: http`へ置き換える。 |
| SDKの開始は成功するが次の接続が失敗する | Locationがループバックかホスト専用のURL | コンテナ向けに`GCS_MIMIC_API_BASE_URL: http://gcs:4443`を指定し、サービスを再作成する。 |
| ブラウザーが`gcs`を名前解決できない | コンテナDNSはホストでは使えない | この例では`http://localhost:14443`を使い、署名なしセッションのpath/queryを保って変換する。 |
| 後から足した空ディレクトリがバケットにならない | seedは初回で完了している | バケットファイルへ`{"name":"exports"}`を追加して再起動する。データボリュームは消さない。 |
| HTTPで接続できない・probeがunhealthy | スキームや待受設定の不一致 | `GCS_MIMIC_SCHEME: http`と`GCS_MIMIC_LISTEN: 0.0.0.0:4443`を使い、probeにも継承させる。 |
| 一覧でprojectの例つき400が返る | nativeではprojectが必須 | `/storage/v1/b?project=local-project`を使う。既存のlegacyプロファイルは省略時にseed_projectを使う。 |
| ホスト側ポートが使用中 | 別の作業ツリーと重複 | `GCS_MIMIC_PORT=24443`と別プロジェクト名を指定し、ブラウザーのURLも変更する。内部URLは変えない。 |
| 取り消しの`curl --fail`が499で失敗する | JSON resumableの取り消し成功は499 | 上の例のように`%{http_code}`で検査し、取り消しに限って499を成功とする。 |

英語版と同じく、移行元の比較ではコピー再実行・preflight・取り消し後の応答とRubyメタデータに
差が残っています。アプリのコードを変えずに全面移行できるとは主張しません。
既存ストレージの取り込み、memory backend、Go埋め込み、公開イメージ、実ブラウザーの検証は対象外です。
