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

| gRPCの移行元 | gcs-mimicでの対応 |
| --- | --- |
| fake-gcs-server v1.56.1のStorage v1 | `grpc_listen`を指定した場合だけ別ポートの平文h2cを開きます。Go SDKは`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444`で接続します。Storage v2のバケットCRUD・更新・IAM・保持ロック、オブジェクト入出力・一覧・削除・復元、resumable・rewrite・compose・move・双方向範囲読み取りに部分対応しています。CancelResumableWriteでresumableセッションを取り消せます。Rapid appendと未対応フィールドはErrorInfo付き`UNIMPLEMENTED`です。同じエンドポイントで比較対象の旧Storage v1の12 RPCを提供し、プロジェクトを省略したバケット一覧にも対応します。v2をv1の代替とは数えません。[旧v1の契約](grpc-v1-legacy.md)を参照してください。 |

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
| 空のバケットディレクトリを置く`-data /data` | `GCS_MIMIC_BUCKETS_FILE: /buckets.json`と読み取り専用マウント | `./data:/data:ro`のマウントだけで起動のたびにseedを反映し、空のディレクトリもバケットになります。バケットにCORSやバージョニングを設定するには、バケットファイルを使います。 |

`data/assets/`と`data/exports/`の代わりに、起動前に`buckets.json`を作ります。

| バケットメタデータの操作 | gcs-mimicでの動作 |
| --- | --- |
| Ruby `bucket.update { \|b\| b.public_access_prevention = "enforced" }`、JSONバケットPUT | 指定した設定だけを変更し、省略したlabels・versioningなどの設定を保持します。labels:nullで消去し、versioning:{}で無効化します。実GCSでは未検証のローカル方針です。[PUTの契約](bucket-metadata-put.md)を参照してください。 |

| 保持ポリシーの操作 | gcs-mimicでの動作 |
| --- | --- |
| Go `BucketAttrs.RetentionPolicy`、Ruby `bucket.retention_period = 3600` | JSON APIで保持設定を保存し、保持中の削除・上書きを403で拒否します。 |
| Go `LockRetentionPolicy`、Ruby `bucket.lock_retention_policy!` | metageneration条件と更新権限が必要です。ロック後の削除・短縮は400 badRequestExceptionになり、原因と延長例を返します。IAM拒否は403 forbiddenです。実バケットでは試験しません。 |

新しい設定は不要です。[保持ポリシーの対応範囲](sdk-retention-policy.md)を参照してください。

| soft deleteの操作 | gcs-mimicでの動作 |
| --- | --- |
| Go `BucketAttrs.SoftDeletePolicy`、Ruby `bucket.soft_delete_policy=` | JSON APIで0または604800～7776000秒を設定できます。作成時の省略はGCSの既定7日と異なり無効ですが、PATCH・PUTで省略した場合は既存設定を保持します。 |
| 削除済み一覧・世代指定のメタデータ取得・restore | 古い本文を期限まで保持し、復元では新世代を作ります。元の削除済み世代は残ります。省略と0はいずれも一覧200となり、保持中の世代がなければ空です。該当世代がなければget・restoreは404です。ポリシーなしを400にするGoogleの仕様とは異なります。ホールドと保持ポリシーによる拒否も引き続き適用します。 |
| 削除済み世代だけのバケットを削除 | バケットと保持中の世代を完全削除します。バケット自体のsoft deleteと初期化ファイルの設定は未対応です。[対応範囲](sdk-soft-delete-restore.md)を参照してください。 |

既存の`bucket`へ、削除前に7日のポリシーを付けます。固定したGo Storage v1.70.0と
Ruby Storage 1.62.1のSDKテストで確認した呼び方です。
Goでは`storage`・`time`のimportと既存の`ctx`を前提にします。

```go
_, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}})
```

```ruby
bucket.soft_delete_policy = { retention_duration_seconds: 604800 }
```

Goでは削除前に`err`を確認してください。未設定での`softDeleted=true`は空の200を返します
（公式は400）。これはオーナー承認済みのローカル仕様です。
無効化前から保持している世代は引き続き一覧に出ます。
`--log-level debug`では、ポリシー未設定の空一覧に理由と
`softDeletePolicy.retentionDurationSeconds=604800`の修正例を出します。

```json
{"version":1,"project":"local-project","buckets":[{"name":"assets"},{"name":"exports"}]}
```

空ディレクトリのseedマウントは外し、このファイルを渡します。
再起動時は未作成のバケットだけを追加し、既存のオブジェクトやAPIで変更した設定を保ちます。
ファイルから項目を消してもバケットは削除しません。次のバケットはファイルに追加して再起動します。
オブジェクトの初期ファイルは別の[初期ファイルのディレクトリ](container-usage.md#初期ファイルを読み込む)で扱います。

`gcs`サービスを公開イメージに置き換え、アプリのサービスはそのままにします。
`buckets.json`はComposeファイルの隣に置きます（マウント元はComposeファイルからの相対パスです）。
例のファイル名は`compose.migration.yaml`です。既存アプリでは、自分のComposeファイルのサービスを書き換えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.3.0
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

## アプリ側の資格情報でRails Active Storageを使う

データ用の`endpoint`を変えてもOAuthの取得先は変わりません。
googleauth 1.17.6はサービスアカウントJSONの`token_uri`を読みません。
Rails 8.1.4の`storage.yml`では資格情報の生成引数`token_credential_uri`を渡せず、
`external_account`には既定のV2署名に必要な秘密鍵がありません。
エミュレーターにもトークン発行経路はなく、利用できる取得先パスはありません。

隔離した**permissive**環境では、アプリ側でダミートークンを設定した
サービスアカウント資格情報を作り、サービスへ渡せます。
JSONの置き場所・プロジェクト・メールはすべて例です。
ローカルで生成したテスト鍵を使い、実際のGoogleの資格情報は使わないでください。

```ruby
require "googleauth"
require "active_storage/service/gcs_service"

credentials = File.open("config/credentials/example-local-service-account.json") do |key|
  Google::Auth::ServiceAccountCredentials.make_creds(
    json_key_io: key,
    scope: "https://www.googleapis.com/auth/devstorage.full_control"
  )
end
credentials.access_token = "example-local-token"
credentials.expires_at = Time.now + 86_400
service = ActiveStorage::Service::GCSService.new(
  project: "example-project", bucket: "example-bucket", credentials: credentials,
  endpoint: "http://127.0.0.1:4443/"
)
```

アプリが作ったこのサービスでローカルの操作を行います。例のJSONには
`type: service_account`、`project_id: example-project`、
`client_email: example-signer@example.invalid`とローカル生成のRSA秘密鍵`private_key`が必要です。
`example-bucket`も事前に作成します。新しいトークンでOAuth取得を避けつつ、鍵をV2署名に使えます。
クライアントが再取得を必要とする前にトークンと有効期限を更新してください。
再取得時は通常のOAuth接続を試みます。
permissiveではダミーのBearerトークンを検証せず受け付けます。

外部通信を防ぐ既存のRails 8.1.4・Storage 1.62.1のシナリオで、内部認証を外さずに
アップロード・取得・削除と変更なしのV2署名GETを確認しました。
確認したのはサービスを直接呼ぶ操作だけで、`Blob.service`の差し替えや
`storage.yml`への名前付きサービスの登録を含め、`ActiveStorage::Blob`や添付から使う配線は未確認です。
これはアプリ側の個別レシピで、通常の`storage.yml`認証は引き続き失敗します。
Railsのstrict構成と自動更新は未確認です。[検証と制限](../en/slices/sdk-local-auth-softdelete-notes.md)と
SPEC-ACTIVE-STORAGE-LOCAL-CREDENTIALSを参照してください。DESIGN-084はPARTIALのままです。

## 移行で困ったとき

| 症状 | 原因 | 直し方 |
| --- | --- | --- |
| `-scheme`や`FAKE_GCS_SCHEME`で起動が止まる | 旧設定名は別名ではない | 古いcommandやキーを消し、`GCS_MIMIC_SCHEME: http`へ置き換える。 |
| SDKの開始は成功するが次の接続が失敗する | Locationがループバックかホスト専用のURL | コンテナ向けに`GCS_MIMIC_API_BASE_URL: http://gcs:4443`を指定し、サービスを再作成する。 |
| ブラウザーが`gcs`を名前解決できない | コンテナDNSはホストでは使えない | この例では`http://localhost:14443`を使い、署名なしセッションのpath/queryを保って変換する。 |
| 後から足した空ディレクトリがバケットにならない | 0.2.1以前は初期ファイルを一度だけ読み込んでいた。[0.3.0のリリースノート（英語）](../en/releases/0.3.0.md)を参照 | 0.3.0へ更新すると起動のたびにseedを反映するため、再起動でバケットが作られる。またはバケットファイルへ`{"name":"exports"}`を追加して再起動する。データボリュームは消さない。 |
| HTTPで接続できない・probeがunhealthy | スキームや待受設定の不一致 | `GCS_MIMIC_SCHEME: http`と`GCS_MIMIC_LISTEN: 0.0.0.0:4443`を使い、probeにも継承させる。 |
| 一覧でprojectの例つき400が返る | nativeではprojectが必須 | `/storage/v1/b?project=local-project`を使う。既存のlegacyプロファイルは省略時にseed_projectを使う。 |
| ホスト側ポートが使用中 | 別の作業ツリーと重複 | `GCS_MIMIC_PORT=24443`と別プロジェクト名を指定し、ブラウザーのURLも変更する。内部URLは変えない。 |
| 取り消しの`curl --fail`が499で失敗する | JSON resumableの取り消し成功は499 | 上の例のように`%{http_code}`で検査し、取り消しに限って499を成功とする。 |

英語版と同じく、移行元の比較ではコピー再実行・preflight・取り消し後の応答とRubyメタデータに
差が残っています。アプリのコードを変えずに全面移行できるとは主張しません。
既存ストレージの取り込み、memory backend、Go埋め込み、公開イメージ、実ブラウザーの検証は対象外です。
