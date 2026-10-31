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

公開イメージの例は0.7.0です。開発中ソースのRAM保存、ローカルPub/Sub、HTTP batch、
XML一覧・コピー・multipart、管理・reset・reseed、strict JWT、追加SDKアダプターには
ソースからのビルドが必要です。[機能と検証の対応表（英語）](../en/evidence/migration-integration.md)では、
実施済みの通信テストと、未実施の最終SDK・コンテナ・ブラウザー検証を分けています。
機能の実装状態は共通の台帳を原本とし、全面置換は未達のままです。

## GitHubで確認した利用例と移行時の違い

以下はリンク先のcommitでソースを確認した結果です。各アプリ自体をこのソースに接続して
実行した結果ではなく、GitHub全体を網羅した調査でもありません。
古い参照版やforkの挙動を、upstream v1.56.1やGoogleの仕様と同一視しません。

| 利用例 | 移行時の違いと今回の対応 |
| --- | --- |
| [gcsfuse](https://github.com/GoogleCloudPlatform/gcsfuse/blob/4fefe6330742a00f4706cf48cae01a15b4766e86/internal/storage/fake_storage_util.go) | InitialObjectsの明示世代・metadata・gzip本文を使います。単なるseedでは不足するため、Go facadeから管理用の共通公開処理へ渡します。 |
| [Apache Beam](https://github.com/apache/beam/blob/e843314a31dad586cdb585a7f96a67499a448e97/sdks/go/pkg/beam/io/filesystem/gcs/gcs_test.go) | NewServer・Client・Stopを使います。Go facadeで対応する範囲はRust子プロセスであり、同一プロセスへの組み込みではありません。 |
| [go-feature-flag](https://github.com/thomaspoignant/go-feature-flag/blob/d6ef445437b5c644e3acb4540e0d4d8646289fb4/testutils/gcs_mock.go) | CreateObjectへ本文とMD5を渡します。fixtureも共通のチェックサム・条件・公開処理で検証します。 |
| [TiDB](https://github.com/pingcap/tidb/blob/49ad420278001b896de85fd7985b35ff49be877f/pkg/objstore/gcs_test.go) | NoListener・HTTPClient・CreateBucketWithOptsを使います。Unix HTTPと3フィールドのバケットhelperを提供し、既存値を更新しない重複規則を保ちます。Windows NoListenerや全Backend APIには対応していません。 |
| [Node Testcontainers](https://github.com/testcontainers/testcontainers-node/blob/b99bb381be1c2470691a533937c23efd25389953/packages/modules/gcloud/src/cloudstorage-emulator-container.ts) | 実行ファイル名・ログ待機・起動後endpoint更新を固定しています。通常entrypoint・healthcheck・ランダムportを使うgeneric adapterが必要です。 |
| [.NET Testcontainers](https://github.com/testcontainers/testcontainers-dotnet/blob/a6ce741af4dde9e8eb5f8c49bf2b90040b981187/src/Testcontainers.FakeGcsServer/FakeGcsServerBuilder.cs) | shellと旧バイナリを固定したsealed builderは、イメージの差し替えだけでは動きません。独立したgeneric adapterを使います。 |
| [Aiven Java wrapper](https://github.com/Aiven-Open/testcontainers-fake-gcs-server/blob/ec7c605b593d648527f29b3df65cdfacda93985a/src/main/java/io/aiven/testcontainers/fakegcsserver/FakeGcsServerContainer.java) | 旧バイナリと内部config更新を前提とします。通常entrypointと明示的な管理APIを使うgeneric adapterへ移行します。Java Testcontainers本体の専用moduleとは別です。 |
| [Arrow object_store](https://github.com/apache/arrow-rs-object-store/blob/a0dc669ecf50aee37ec4f8e19dbd759f7028613f/.github/workflows/ci.yml) | CIでforkを使い、XMLや世代条件に比較対象固有の制限があります。独立したArrow harnessで確認し、公式Google Rust SDKの結果と混同しません。 |

追加アダプターの範囲と最終実行の条件は[契約（英語）](../en/slices/sdk-client-adapters.md)にあります。
Railsは既存の検証優先対象です。このconsumer調査で代表的なRailsアプリの一次ソースを
確認したとはしておらず、公式SDK・Active Storageと本プロジェクトのharnessで別に検証します。

## fake-gcs-serverのComposeを置き換える

| gRPCの移行元 | gcs-mimicでの対応 |
| --- | --- |
| fake-gcs-server v1.56.1のStorage v1 | `grpc_listen`を指定した場合だけ別ポートの平文h2cを開きます。Go SDKは`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444`で接続します。Storage v2のバケットCRUD・更新・IAM・保持ロック、オブジェクト入出力・一覧・削除・復元、resumable・rewrite・compose・move・双方向範囲読み取りに部分対応しています。CancelResumableWriteでresumableセッションを取り消せます。appendableオブジェクトは作成・flush・追記中の読み取り・takeover・確定に対応します（[Rapid appendの契約](grpc-v2-rapid-append.md)）。未対応フィールドはErrorInfo付き`UNIMPLEMENTED`です。同じエンドポイントで比較対象の旧Storage v1の12 RPCを提供し、プロジェクトを省略したバケット一覧にも対応します。v2をv1の代替とは数えません。[旧v1の契約](grpc-v1-legacy.md)を参照してください。 |

既存アプリのサービスと`STORAGE_EMULATOR_HOST=gcs:4443`はそのまま残し、
`gcs`サービスを置き換えます。よく使う5組のフラグは次のように書き換えます。
移行前のCompose全体は[英語版の例](../en/fake-gcs-server-migration.md#replace-a-fake-gcs-server-compose-service)を参照してください。
0.7.0以降は、古いフラグと`FAKE_GCS_*`も書き換えずに動き、置き換え先を警告で表示します。
この手順ではnativeの名前を使います。[旧設定の読み替え（英語）](../en/slices/legacy-setting-aliases.md)を参照してください。

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

`./data:/data:ro`のマウントだけでも動きます。イメージは起動のたびにこれをseedとして反映するため、
後から足したものも含めて空のディレクトリがバケットになります。この手順ではそのマウントを外してファイルを渡します。
ファイルにはCORSやバージョニングなどのバケットの設定も書けます。
再起動時は未作成のバケットだけを追加し、既存のオブジェクトやAPIで変更した設定を保ちます。
ファイルから項目を消してもバケットは削除しません。次のバケットはファイルに追加して再起動します。
オブジェクトの初期ファイルは別の[初期ファイルのディレクトリ](container-usage.md#初期ファイルを読み込む)で扱います。

`gcs`サービスを公開イメージに置き換え、アプリのサービスはそのままにします。
`buckets.json`はComposeファイルの隣に置きます（マウント元はComposeファイルからの相対パスです）。
例のファイル名は`compose.migration.yaml`です。既存アプリでは、自分のComposeファイルのサービスを書き換えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.7.0
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

fake-gcs-serverはすべてのルートに同じワイルドカードのCORSを返すため、
`$api/assets/browser.txt`のようなpath-styleの公開URLを設定なしでfetchできました。
gcs-mimicでは、バケットに`cors`を設定するまでこのfetchはCORSエラーになります。
移行中にこれまでの挙動を保つには、サーバーの環境変数に`GCS_MIMIC_CORS_FALLBACK=allow-all`を追加します。
適用されるのはCORSルールを保存していないバケットだけで、権限は変わりません。
無効のまま、Origin付きのXMLリクエストがルールのないバケットに届くと、
バケットごとに1回だけ`cors_unconfigured`の警告をログに出します。
詳しくは[CORSフォールバックの仕様](../en/slices/cors-fallback.md)（英語）を参照してください。

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

## Rails Active Storage

データ用の`endpoint`を変えても、OAuthの取得先は変わりません。
googleauth 1.17.6は、既定のuniverseのサービスアカウントではGoogleにトークンを取りに行き、`token_uri`も読みません。
代わりに、Google以外の`universe_domain`を書いた`gcs_emulator`項目を`config/storage.yml`に足します。
するとgoogleauthは自分でトークンに署名し、Googleには接続しません。
開発・テストではこの項目を選び、本番は本番用の項目のままにします。
[Railsの手順](rails-active-storage.md)と[検証結果](sdk-active-storage-scenario.md#storageymlだけでエミュレーターを使う)を参照してください。

以前のレシピは、アプリのコードでトークンと1日の有効期限を設定してサービスを組み立てていました。
期限が切れるとgoogleauthがGoogleにトークンを更新しに行くため、置き換えました。
この項目はpermissiveプロファイルでだけ使えます。

## 移行で困ったとき

| 症状 | 原因 | 直し方 |
| --- | --- | --- |
| `-scheme`や`FAKE_GCS_SCHEME`で起動が止まる | 0.6.0以前は旧設定名を読み替えない | 0.7.0以降に上げるか、古いcommandやキーを消して`GCS_MIMIC_SCHEME: http`へ置き換える。 |
| `-event.pubsub-topic`などの`FAKE_GCS_EVENT_*`で起動が止まる | イベント通知は未対応 | イベント設定を消す。通知が必要なテストはまだgcs-mimicでは動かせない。 |
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
