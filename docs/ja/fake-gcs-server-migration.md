# fake-gcs-serverからのCompose移行

[English](../en/fake-gcs-server-migration.md) ·
[検証と操作の対応表](../en/slices/migrate-guide-compose.md)

移行するかどうかの判断と入れ替えの手順は、先に[fake-gcs-serverから移行する](migrate-from-fake-gcs-server.md)を読んでください。このページは詳しい資料です。

対象はUX-012、DESIGN-061、UX-001です。既定のpermissive/nativeで、HTTP、
空バケットの初期化、Go SDK、署名のないブラウザー転送を確認しています。
UX-012とUX-001はPARTIAL、DESIGN-061は既存のホスト・コンテナ接続範囲で
IMPLEMENTEDです。永続データの移行やfake-gcs-serverの全面的な置き換えは含みません。

公開イメージの例は0.14.0です。0.8.0以降は、RAM保存、ローカルPub/Sub、HTTP batch、XML一覧・コピー・multipart、管理・reset・reseed、strict JWT、追加SDKアダプターも使えます。[機能と検証の対応表（英語）](../en/evidence/migration-integration.md)に、各機能の対応範囲と検証結果をまとめています。
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
0.7.0以降は、古いフラグも書き換えずに動きます。0.14.0より後のバージョンは`FAKE_GCS_*`環境変数を読みません。
この手順ではnativeの名前を使います。[オプションの別名（英語）](../en/slices/option-aliases.md)を参照してください。

| 書き換え前 | 書き換え後 | 必要な理由・違い |
| --- | --- | --- |
| `-scheme http` | `GCS_MIMIC_SCHEME: http` | 省略時はHTTPSなので、既存のHTTPクライアントが接続できません。イメージのhealthcheckもこの値に従います。 |
| `-host 0.0.0.0 -port 4443` | 指定なし | イメージは最初から`0.0.0.0:4443`で待ち受けます。ループバックだけで待ち受けると他のコンテナから接続できないため、この既定が必要です。 |
| `-public-host gcs:4443 -external-url http://gcs:4443` | 指定なし | resumableのLocationなど、サーバーが返すリンクは各リクエストのHostに従います。コンテナのSDKは`gcs:4443`を、ホストは公開ポートを受け取ります。nativeのpath-style読み取りにはpublic-host設定が不要です。旧来の仮想ホスト判定や公開権限は再現しません。 |
| `-backend filesystem -filesystem-root /storage` | `/var/lib/gcs-mimic`へ名前付きボリューム | 既定はSQLiteと内部blobの永続ストレージです。保存形式が違うため、移行元のボリュームは別に保管します。`-backend memory`は上限付きのRAM保存に読み替えますが、この手順では使いません。 |
| 空のバケットディレクトリを置く`-data /data` | `GCS_MIMIC_BUCKETS_FILE: /buckets.json`と読み取り専用マウント、または同じ文書をインラインで書く`GCS_MIMIC_BUCKETS`（[後述](#バケットファイルをcomposeyamlに書く)） | `./data:/data:ro`のマウントだけで起動のたびにseedを反映し、空のディレクトリもバケットになります。バケットにCORS、バージョニング、IAMポリシーを設定するには、バケットの文書を使います。 |

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
| Go `BucketAttrs.SoftDeletePolicy`、Ruby `bucket.soft_delete_policy=` | JSON APIで0または1～3155760000秒を設定できます。soft deleteはfake-gcs-serverと同じく、すべての保存方式で既定では無効です。作成時に省略するとポリシーを保存せず、**上書きや削除の直後に本文を解放します**。残すには`softDeletePolicy.retentionDurationSeconds`を指定するか、サーバーを`--default-soft-delete-retention-seconds N`（または`GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS=N`）で起動します。この既定と範囲はローカル独自の仕様です。Cloud Storageの既定は7日で、受け付けるのは0または7～90日なので、7日未満を拒否する動作はここでは試せません。ポリシーを保存済みのバケットは、0.11.0までが新規バケットに付けた7日も含めてそのまま残ります。PATCH・PUTで省略した場合は既存設定を保持します。`--compat-profile legacy-emulator`は引き続き既定を適用しません。 |
| 削除済み一覧・世代指定のメタデータ取得・restore | 古い本文を期限まで保持し、復元では新世代を作ります。元の削除済み世代は残ります。ポリシーを持たないバケットと0を明示したバケットはいずれも一覧200となり、保持中の世代がなければ空です。該当世代がなければget・restoreは404です。ポリシーなしを400にするGoogleの仕様とは異なります。ホールドと保持ポリシーによる拒否も引き続き適用します。 |
| 削除済み世代だけのバケットを削除 | 正の保持期間があれば、バケットは保持中の世代ごとsoft deleteされ、generationを指定して復元できます（`buckets.restore`）。オブジェクトはsoft deleteされたままで、別途復元します。保持期間がなければ完全削除します。初期ファイルやバケットファイルで作るバケットにはサーバーの既定を使い、バケットファイルの項目では`softDeletePolicy`も指定できます。[対応範囲](sdk-soft-delete-restore.md)を参照してください。 |

既存の`bucket`のオブジェクトを復元できるようにするには、削除前にポリシーを付けます。7日はCloud Storageの既定と同じ値です。固定したGo Storage v1.70.0と
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
ファイルにはCORS、バージョニング、IAMポリシーなどのバケットの設定も書けます。
再起動時は未作成のバケットだけを追加し、既存のオブジェクトやAPIで変更した設定を保ちます。
ファイルから項目を消してもバケットは削除しません。次のバケットはファイルに追加して再起動します。
オブジェクトの初期ファイルは別の[初期ファイルのディレクトリ](container-usage.md#初期ファイルを読み込む)で扱います。

`gcs`サービスを公開イメージに置き換え、アプリのサービスはそのままにします。
`buckets.json`はComposeファイルの隣に置きます（マウント元はComposeファイルからの相対パスです）。
例のファイル名は`compose.migration.yaml`です。既存アプリでは、自分のComposeファイルのサービスを書き換えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.14.0
    ports:
      - "127.0.0.1:${GCS_MIMIC_PORT:-4443}:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - migration-data:/var/lib/gcs-mimic
      - ./buckets.json:/buckets.json:ro
volumes:
  migration-data:
```

接続数の上限、オブジェクトサイズ、タイムアウト、soft deleteのための設定は要りません。
リンクは各リクエストのHostと接続のスキームから組み立てるので、1台のサーバーでも、コンテナのSDKは`gcs:4443`を、
ホストやブラウザーは`localhost`と公開ポートを受け取ります。
`GCS_MIMIC_API_BASE_URL`は通知先を1つに固定するときだけ指定します。
転送ヘッダーは無視するので、Hostを書き換えるリバースプロキシでは指定が必要です。
リンクが常に固定される0.11.0までのリリースでは、ここに`GCS_MIMIC_API_BASE_URL: http://gcs:4443`を追加してください。
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
内部ポートは4443のままです。名前付きボリュームもプロジェクト別です。
同じファイル・プロジェクトを指定して`docker compose down`で停止すると、データは残ります。

### バケットファイルをcompose.yamlに書く

バケットの文書はComposeファイルの中にも書けるので、別の`buckets.json`は要りません。
`GCS_MIMIC_BUCKETS_FILE`と`buckets.json`のマウントの代わりに、サービスのenvironmentへ`GCS_MIMIC_BUCKETS`として書きます。2つの設定は同時に使えません。
これが推奨の形で、0.14.0以降が必要です。

```yaml
services:
  gcs:
    environment:
      GCS_MIMIC_BUCKETS: |
        {"version": 1, "buckets": [
          {"name": "app-images", "cors": [{"origin": ["http://localhost:${FRONTEND_PORT:-8081}"],
            "method": ["GET", "HEAD", "PUT"], "responseHeader": ["Content-Type"], "maxAgeSeconds": 3600}],
           "iamPolicy": {"bindings": [{"role": "roles/storage.objectViewer", "members": ["allUsers"]}]}},
          {"name": "app-exports"}
        ]}
```

`${FRONTEND_PORT:-8081}`を展開するのはgcs-mimicではなくComposeで、
[変数展開の規則](https://docs.docker.com/reference/compose-file/interpolation/)に従います。
gcs-mimicは結果をそのままJSONとして読み、何も展開しません。`$`そのものは`$$`と書きます。
`iamPolicy`は新しいバケットにポリシーを付け、`setIamPolicy`と同じく既定のポリシーを置き換えます。
0.13.0までのリリースは未対応として拒否します。

名前付きボリュームは`docker compose down`の後も残るので、次回起動ではバケットが既にあります。
インラインの文書ではこうしたバケットにも反映します。`GCS_MIMIC_BUCKETS_FILE_MODE`の既定値が`reconcile`なので、
各起動は項目が書いたフィールドのうち値が異なるものだけを書き込み、書かないフィールドはAPIでの変更を保ち、
変更したバケット数をreadyの`buckets_file.reconciled`に数えます。既存バケットをそのまま残すには、モードを`create`にします。
値はサービスのenvironmentの一部なので、編集した後は`docker compose up -d`だけでComposeがコンテナを作り直し、次の起動で反映します。
`docker compose restart`では古い値のままです。
フィールドを消しても元には戻りません。上の`iamPolicy`を消しても
`app-images`は公開のままで、`iamPolicy`を書かなくなったことを一度だけ警告します。
望むポリシーを項目に書くまで、この状態が続きます。

別の方法として、トップレベルの`configs:`に`content:`で、`GCS_MIMIC_BUCKETS_FILE`が指すファイルの中身を書くこともできます。
サービスの`configs:`で`target`のパスへマウントします。
[Composeのconfigsリファレンス](https://docs.docker.com/reference/compose-file/configs/)によると、
`content`はDocker Compose 2.23.1以降で使えます。
また、サービスに`read_only: true`を付けないでください。Composeは中身をコンテナへコピーするため、読み取り専用のコンテナでは作成を拒否します。
バケットファイルの既定値は`create`で、既存バケットの編集はstderrの警告とreadyの`unapplied`で知らせるだけなので、
反映するには`GCS_MIMIC_BUCKETS_FILE_MODE: reconcile`を追加します。

```yaml
services:
  gcs:
    environment:
      GCS_MIMIC_BUCKETS_FILE: /etc/gcs-mimic/buckets.json
      GCS_MIMIC_BUCKETS_FILE_MODE: reconcile
    configs:
      - source: gcs_buckets
        target: /etc/gcs-mimic/buckets.json
configs:
  gcs_buckets:
    content: |
      {"version": 1, "buckets": [{"name": "app-images"}, {"name": "app-exports"}]}
```

編集した`content`をサーバーが読むのは、作り直したコンテナだけです。
Docker Compose 5.6.1では、`docker compose up -d`だけでは動いているコンテナをそのまま使い、`docker compose restart`では古い中身のままなので、`docker compose up -d --force-recreate gcs`を実行します。

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
path-styleの匿名GETは既定のpermissiveで本文を返します。XMLを別オリジンからfetchする場合は
バケットのCORSルールに従い、ルールのないバケットでは下のフォールバックに従います。strictではCORSを許可しても権限は増えません。
[GoogleのCORS仕様](https://docs.cloud.google.com/storage/docs/cross-origin)も参照してください。

fake-gcs-serverはすべてのルートに同じワイルドカードのCORSを返すため、
`$api/assets/browser.txt`のようなpath-styleの公開URLを設定なしでfetchできました。
既定のpermissiveプロファイルでは、gcs-mimicでもそのままfetchできます。
`GCS_MIMIC_CORS_FALLBACK`の既定値が`allow-all`で、CORSルールを保存していないバケットのXML・公開URLではどのオリジンも許可するからです。
こうした別オリジンのリクエストは、バケットごとに最初の1回だけinfoレベルの`cors_fallback_default`としてログに出すので、ルールが必要になるバケットを確認できます。
ルールのあるバケットは常にそのルールに従い、フォールバックで権限は変わりません。
strictモードの既定値はCloud Storageと同じ`none`で、バケットに`cors`を設定するまでfetchはCORSエラーになります。
[strict localレシピ](../../examples/strict-local/README.md)（英語）は、バケットごとにこのルールを書いています。
詳しくは[CORSフォールバックの仕様](../en/slices/cors-fallback.md)（英語）を参照してください。

ブラウザーのアドレスで開始したresumableセッションのLocationは、そのアドレスになります。
セッションURIはサーバーの別のアドレスでも使えます。`gcs:4443`経由で開始したサーバーがブラウザーへ渡す場合は、
pathとqueryを保ち、到達可能なオリジンへ置き換えます。シナリオはHost変更後のPUT、Content-Range、preflightを確認します。
取り消しのDELETEは[JSON resumableの仕様](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads)どおり
`clientClosedRequest`のJSONエンベロープと**499**を返します。
その後のセッションアクセスも同じ499応答になります。
499を基盤障害と扱わず、取り消しの成功として判定してください。

```sh
location=$(curl --fail -sS -D - -o /dev/null \
  -H 'Content-Type: application/json' -H 'X-Upload-Content-Type: text/plain' \
  -d '{}' "$api/upload/storage/v1/b/assets/o?uploadType=resumable&name=cancelled.txt" \
  | awk 'tolower($1)=="location:" {sub("\r$", "", $2); print $2}')
cancellation_status=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "$location")
test "$cancellation_status" = 499
```

`curl --fail`は499をエラー扱いするため、取り消しではコードを明示的に検査します。
オリジンの置き換えは署名なしのpermissiveセッションに限ります。
strictの署名URLは最終的に利用者がアクセスするURLで署名し、後から書き換えません。

### どの構成を使うか

| 目的 | 構成 | 設定 |
| --- | --- | --- |
| fake-gcs-serverと同じく動かす。すべてのリクエストを許可し、署名は確認せず、ブラウザーはどのバケットも読める | 既定のpermissiveプロファイル | `GCS_MIMIC_SCHEME=http` |
| 本番に近いアクセス。非公開バケットは非公開のまま、公開バケットは`allUsers`で読め、ブラウザーは署名URLを使い、アプリのサーバーからCloud Storageへの呼び出しは変えない | 信頼済みリスナー付きのstrictモード。[strict localレシピ](../../examples/strict-local/README.md)（英語） | `GCS_MIMIC_SCHEME=http`、`GCS_MIMIC_AUTH_MODE=strict`、`GCS_MIMIC_TRUSTED_LISTEN=0.0.0.0:4444`（公開しない）と、バケットごとのポリシーとCORSルールを書いた`GCS_MIMIC_BUCKETS`。アプリがURLに署名するか、Railsのように`test-signer credentials`の鍵を使うなら`GCS_MIMIC_TEST_SIGNER=enabled`も追加 |
| strictモードをまだ使えない間だけ。署名URLは確認し、それ以外は制限しない | 署名を検証するpermissive | `GCS_MIMIC_SCHEME=http`と`GCS_MIMIC_SIGNED_URLS=verify` |

まずはこの手順と同じ既定の構成で始めてください。
本番に近い形で確認するには、strict localレシピのとおりに信頼済みリスナー付きのstrictモードを使います。
コンテナのスモークテストがこのブロックをそのままイメージで動かしており、各行の意味と、レシピが前提にしている既定値はレシピのREADMEで説明しています。
permissiveの`verify`は、strictモードをまだ使えない構成のための途中段階にすぎません。
不正な署名は拒否しますが、検証に通ったリクエストも署名のないリクエストもすべて許可するので、非公開バケットが非公開のままかは確認できません。
表の2行目と3行目の構成には、0.14.0以降が必要です。
2つのプロファイルとfake-gcs-serverの挙動は、[Dockerで使う](container-usage.md#fake-gcs-serverとの違い構成別)で項目ごとに比べています。

### アプリのサーバーを変えずにstrictモードにする

fake-gcs-serverは権限を確認しないため、strictモードに移ると、アプリのサーバー側のクライアントすべてに認証情報が必要になります。
認証情報のないリクエストは`allUsers`の権限しか使えないからです。
[strict localレシピ](../../examples/strict-local/README.md)（英語）を使うと、この変更は要りません。上の`gcs`サービスをレシピのサービスに置き換えます。
アプリのサービスは、Composeネットワーク内の信頼済みリスナーである`STORAGE_EMULATOR_HOST=gcs:4444`に接続し、そこでは認証情報のないリクエストが組み込みの`trusted-app@gcs-mimic.invalid`として扱われます。このIDはバケットのprojectで`roles/storage.admin`を持ちます。
ブラウザーは公開したポートを使い、公開バケットは`allUsers`で読め、非公開バケットは署名URLでだけ使えます。
IDの文書、証明書、鍵の生成はいずれも要りません。
Go API、Rails、フロントエンドをつなぐ例は[アプリ全体の例](../../examples/full-stack/README.md)（英語）にあります。
信頼済みリスナーのポートは公開しないでください。
[信頼済みリスナーの節](container-usage.md#信頼済みリスナーで非公開バケットを守る)で確認の内容を説明しています。
認証情報付きのリクエストはメインのリスナーと同じく認証し、ブラウザーからのリクエストは403になり、IPリテラル、`localhost`の名前、`gcs`のようなドットのない名前、`GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`に書いた名前以外のHostは421になります。

対象になるのは、サーバー自身が送るリクエストだけです。
fake-gcs-serverは署名を確認しないため、ブラウザーに渡すURLをエミュレーターのときだけ署名なしにしているアプリがあります。
そうしたリクエストはstrictでは401になり、permissiveの`GCS_MIMIC_SIGNED_URLS=verify`では確認する署名がないので許可されます。
必要な変更は、レシピの[信頼済みリスナーの対象外の一覧](../../examples/strict-local/README.md#what-the-trusted-listener-does-not-cover)（英語）にあります。

## Rails Active Storage

データ用の`endpoint`を変えても、OAuthの取得先は変わりません。
googleauth 1.17.6は、既定のuniverseのサービスアカウントではGoogleにトークンを取りに行き、`token_uri`も読みません。
代わりに、Google以外の`universe_domain`を書いた`gcs_emulator`項目を`config/storage.yml`に足します。
するとgoogleauthは自分でトークンに署名し、Googleには接続しません。
開発・テストではこの項目を選び、本番は本番用の項目のままにします。
[Railsの手順](rails-active-storage.md)と[検証結果](sdk-active-storage-scenario.md#storageymlだけでエミュレーターを使う)を参照してください。

以前のレシピは、アプリのコードでトークンと1日の有効期限を設定してサービスを組み立てていました。
期限が切れるとgoogleauthがGoogleにトークンを更新しに行くため、置き換えました。
起動時に鍵を生成するこの項目は、permissiveプロファイルでだけ使えます。
信頼済みリスナーを使っても変わりません。この項目は常に自分のトークンを送り、メインのリスナーと同じく認証されるので、strictモードではサーバーが知っている署名者が必要です。
strict localレシピはそのために組み込みのテスト署名者を有効にしており、それを使う項目をREADMEで示しています。

## 移行で困ったとき

| 症状 | 原因 | 直し方 |
| --- | --- | --- |
| `FAKE_GCS_*`環境変数が効かない | 0.14.0より後のバージョンは読みません | 対応するフラグか`GCS_MIMIC_*`環境変数を設定する。 |
| `-scheme`で起動が止まる | [撤回したリリース](release-policy.md#撤回したリリース)（0.6.0以前）は旧設定名を読み替えない | 現行版に上げるか、古いcommandやキーを消して`GCS_MIMIC_SCHEME: http`へ置き換える。 |
| `-event.pubsub-topic`などの`-event.*`フラグで起動が止まる | 0.7.0以前は配信がない。0.8.0以降は完全なローカル配信設定が必要 | project・topicと、明示したローカルPub/Subエミュレーターの接続先を指定し、topicとsubscriptionは別に用意する。[通知契約（英語）](../en/slices/pubsub-notification-delivery.md)を参照。 |
| SDKの開始は成功するが次の接続が失敗する | Locationが`GCS_MIMIC_API_BASE_URL`や`-external-url`で固定したアドレスになっている、またはプロキシがHostを書き換えている | 固定をやめてリンクを各リクエストに従わせるか、すべてのクライアントから届くアドレスを指定し、サービスを再作成する。 |
| ブラウザーが`gcs`を名前解決できない | コンテナDNSはホストでは使えない | この例では`http://localhost:14443`を使う。サーバーが署名なしセッションURIをブラウザーへ渡すときは、path/queryを保って変換する。 |
| 後から足した空ディレクトリがバケットにならない | [撤回したリリース](release-policy.md#撤回したリリース)（0.2.1以前）は初期ファイルを一度だけ読み込んでいた | 空のデータボリュームで現行版に上げる。起動のたびにseedを反映する。撤回したリリースのデータはサポートしない。 |
| HTTPで接続できない・probeがunhealthy | スキームや待受設定の不一致 | `GCS_MIMIC_SCHEME: http`を指定し、`GCS_MIMIC_LISTEN`はイメージの既定`0.0.0.0:4443`のままにして、probeにも継承させる。 |
| 一覧で400 required（Required parameter: project）が返る | nativeではprojectが必須 | `/storage/v1/b?project=local-project`を使う。既存のlegacyプロファイルは省略時にseed_projectを使う。 |
| ホスト側ポートが使用中 | 別の作業ツリーと重複 | `GCS_MIMIC_PORT=24443`と別プロジェクト名を指定し、ブラウザーのURLも変更する。コンテナからの接続先`http://gcs:4443`は変えない。 |
| 取り消しの`curl --fail`が499で失敗する | JSON resumableの取り消し成功は499 | 上の例のように`%{http_code}`で検査し、取り消しに限って499を成功とする。 |
| リクエストが終わらず、クライアントの接続が使われたままで、ログにも何も出ない | クライアントがリクエスト本文（`Content-Length`またはchunked）を宣言したまま送信を止めている。`body_inactivity_timeout_ms`の既定値0では期限切れが起きないため、サーバーは待ち続け、ログに書くこともない | 該当するクライアントを見つけるには、`GCS_MIMIC_BODY_INACTIVITY_TIMEOUT_MS`を例えば`30000`に設定する。その時間本文が届かないと、JSONは408 `uploadBrokenConnection`、XMLは400 `RequestTimeout`で失敗し、サーバーは`connection_timeout`の警告を記録する。遅いクライアントを切らないよう、既定値は0のまま。 |

英語版と同じく、移行元の比較ではコピー再実行・preflight・取り消し後の応答とRubyメタデータに
差が残っています。アプリのコードを変えずに全面移行できるとは主張しません。
この手順の検証には、既存ストレージの取り込み、memory backend、Go埋め込み、公開イメージ、実ブラウザーを含みません。
