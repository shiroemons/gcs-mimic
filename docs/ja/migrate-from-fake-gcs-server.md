# fake-gcs-serverから移行する

[English](../en/migrate-from-fake-gcs-server.md)

gcs-mimicは普段の使い勝手を[fake-gcs-server](https://github.com/fsouza/fake-gcs-server)から受け継いでいるので、
多くのコンテナ構成は数行の変更で移行できます。
移行する価値があるかどうかを決めるのは「できなくなること」なので、このページはそこから始めます。
対象はgcs-mimic 0.27.0とfake-gcs-server 1.56.1です。

gcs-mimicは、すべての構成でそのまま置き換えられるものではありません。

## できなくなること

影響する構成が多い順に、おおまかに並べています。

| 今していること | 移行後 | 代わりにすること |
| --- | --- | --- |
| `/storage`ボリュームにオブジェクトを保存している | 形式が異なり再利用できません。0.18.0以降は`-filesystem-root`で起動が止まります。 | フラグを外し、[中身をコピー](#既存のオブジェクトを持ってくる)するか初期データから入れ直します。 |
| `FAKE_GCS_*`環境変数を設定している | 0.15.0以降は読みません。警告も出ません。`FAKE_GCS_PORT=9000`を残しても4443番で待ち受けるため、9000番に送るクライアントは接続に失敗します。 | [下の表](#設定の対応表)の対応するフラグか`GCS_MIMIC_*`環境変数を使います。 |
| 既存オブジェクトの世代・カスタムメタデータ・Content-Typeに依存している | 現時点では引き継げません。`gcs-mimic import fake-gcs-server`はありますが、0.14.0ではfake-gcs-serverのAPIでアップロードしたオブジェクトすべてで止まります。これらに自動で付く`projectOwner-test-project`というACLを受け付けないためです。 | テストが依存するオブジェクトをAPIで作り直すか、そのデータはfake-gcs-serverに残します。 |
| 保存先のディレクトリでオブジェクトをファイルとして見る・編集する | オブジェクトは中身の分からないファイルとして保存します。`/data`は読み取り専用の入力で、APIの書き込みは反映されません。 | [同期ディレクトリ](#オブジェクトをファイルとして使い続ける)を追加します。オブジェクトとファイルが双方向で揃います。競合と復旧のケースはまだ一部のみの対応です。 |
| `docker exec … sh`、シェルのヘルスチェック、シェルのentrypointを使っている | イメージにはシェルもパッケージマネージャーもありません。 | `healthcheck:`の上書きを外します。イメージに専用のもの（`gcs-mimic healthcheck`）があり、その開始猶予は[同期ディレクトリ](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)の最初の照合にも足ります。スクリプトは別のコンテナから実行します。 |
| rootだけが読み書きできるディレクトリをマウントしている | サーバーはrootではなくUID/GID 10001で動きます。 | `/var/lib/gcs-mimic`には名前付きボリュームを使い、`/data`のファイルは全ユーザーが読めるようにします。 |
| Goのテストプロセスに`fakestorage`を組み込んでいる | プロセス内で動くサーバーはありません。[`fakestorage`パッケージ（英語）](../../go/README.md#fakestorage)はRustのバイナリを子プロセスとして起動し、APIの一部に対応します。`Backend()`は使えません。 | この範囲で足りる場合は、importパスを`github.com/shiroemons/gcs-mimic/go/fakestorage`に変えます。新しく書くテストには、テストごとのバケット、初期データの登録、起動直後の状態に戻す機能を備えた[`gcsmimic`（英語）](../../go/README.md#gcsmimic)を使います。 |
| `fake-gcs-server`を決め打ちしたTestcontainersのモジュールやラッパーを使っている | ラッパーが旧実行ファイルと起動ログを前提にしているため、イメージの変更だけでは動きません。 | [汎用コンテナのアダプター（英語）](../en/slices/sdk-client-adapters.md)を使います。 |
| `-backend memory`で大量のデータを扱う | RAM保存には上限があり、既定ではオブジェクト本体で使えるメモリの約半分（64 MiB〜1 GiB）です。超えると書き込みが503で失敗します。 | `memory_max_bytes`を増やす（[RAM保存の仕様（英語）](../en/slices/memory-backend.md)）か、ディスク保存を使います。 |
| 初期ファイルのディレクトリに`.metadata`ファイルを置いている | メタデータとしては読まず、通常のオブジェクトとして取り込みます。 | メタデータはAPIで設定します。 |
| `/_internal/reseed`や`PUT /_internal/config`を呼んでいる | 既定では無効です。 | 他から届かない待受で`GCS_MIMIC_ADMIN_ENABLED=true`を設定します。 |
| `?project=`なしでバケットを一覧する | Cloud Storageと同じく400を返します。SDKは最初からプロジェクトを送ります。 | `?project=local-project`を付けるか、`GCS_MIMIC_COMPAT_PROFILE=fake-gcs-server`を設定します。 |
| Pub/Subエミュレーターなしで`-event.*`を使う | 起動に失敗します。通知を待つテストが止まらないようにするためです。 | `PUBSUB_EMULATOR_HOST`でローカルのエミュレーターを指定します。 |
| `-public-host`のHostヘッダー照合に依存している | 受け付けます。サブドメインは仮想ホストのバケットを選びます。 | JSONには別ホストか`--api-base-url`を使います。 |
| Cloud Storageなら拒否されるリクエストを送っている | gcs-mimicはCloud Storageと同じように入力を検証するので、4xxになることがあります。 | リクエストを直します。 |
| resumableアップロードの取り消しのステータスを検査している | Googleが取り消し成功として定めている499を返します。 | 499を受け入れます。 |

## fake-gcs-serverのままがよい場合

次のどれかに当てはまるなら、今は移行しないほうがよいでしょう。

- Goのテストに`fakestorage`を組み込み、`Backend()`やプロセス内での実行に依存している。
- `/storage`ボリュームにある既存オブジェクトの世代やカスタムメタデータが必要。
- APIが応答した瞬間にオブジェクトのファイルを読み、待てないテストがある。[同期ディレクトリ](#オブジェクトをファイルとして使い続ける)はファイルを応答の少し後に書くので、先に`gcs-mimic sync wait`が要ります。

## コンテナを入れ替える

```diff
 gcs:
-  image: fsouza/fake-gcs-server:1.56.1
+  image: ghcr.io/shiroemons/gcs-mimic:0.27.0
-  command: ["-scheme", "http", "-external-url", "http://gcs:4443"]
+  command: ["--api-base-url", "http://gcs:4443"]
   volumes:
     - ./data:/data
-    - gcs-storage:/storage
+    - gcs-data:/var/lib/gcs-mimic
```

`command:`のフラグと`./data:/data`のマウントは、多くがそのまま動きます。
0.18.0以降は、意味が変わるフラグで起動を止め、nativeの書き方を案内します。
`-scheme http`は外せます。既定で同じポートがHTTPとHTTPSを受け付けます。
HTTPでエミュレーターに接続するアプリのコードは、`STORAGE_EMULATOR_HOST`も含めて変わりません。

ソフトデリートはfake-gcs-serverと同じく既定で無効です。オブジェクトを削除・上書きすると、本文はすぐに消えます。
残すには`GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS`を設定します。

## 既存のオブジェクトを持ってくる

`/storage`ボリュームのオブジェクトは普通のファイルなので、名前と中身を初期ファイルとして使えます。
先にfake-gcs-serverを止めてから、コピーします。

```sh
docker run --rm -v OLD_VOLUME:/storage:ro -v "$PWD/data:/out" busybox:1 \
  sh -c 'cp -R /storage/. /out/ && rm -f /out/*.bucketMetadata && chmod -R a+rX /out'
```

`./data:/data:ro`をマウントしてgcs-mimicを起動します。
空のバケットも含めたバケット、オブジェクト名、中身を引き継げます。
Content-Typeはファイルの拡張子から決め直します。世代、カスタムメタデータ、ACLは引き継げません。

初期ファイルは起動のたびに反映され、同じ名前のオブジェクトへのAPIでの変更を置き換えます。
オブジェクトがデータボリュームに入ったあと、この動きが不要ならマウントを外します。

## オブジェクトをファイルとして使い続ける

fake-gcs-serverのfilesystemバックエンドは、オブジェクトを`<root>/<bucket>/<object>`のファイルとして保存します。
同期ディレクトリを使うと、gcs-mimicでも同じ見え方になり、しかも双方向です。
すべてのバケットと現行オブジェクトがホストのディレクトリに`BUCKET/OBJECT`のファイルとして現れ、
そこへ置いたり編集したりしたファイルはオブジェクトになり、ファイルを消すとオブジェクトも消えます。

最初に起動する前に、3つを済ませます。

- **ディレクトリを先に作ります。** 無いとサーバーは起動しません。Dockerは、無いbind元をホストに作りません。
- **Linuxのホストでは、サーバーを自分のユーザーで動かします。** サーバーは自分のユーザーだけが読めるモード（ファイルは`0600`、ディレクトリは`0700`）で書き出し、
  自分のユーザーが所有するディレクトリにしか書けません。UID 10001のままだと、書き出されたファイルは`sudo`なしでは読めません。
  そこで自分のUIDを渡し、データも自分が所有するホストのディレクトリに置きます（既定の名前付きボリュームはUID 10001の所有です）。
  macOSのDocker Desktopは所有者を対応付けるので、macOSでは不要です。
- **gitの対象から外します。** ディレクトリにはオブジェクトのほかにサーバーのマーカーファイルも入ります。
  `/sync/`（選んだディレクトリ）を`.gitignore`に追加してください。

自分のサービスに、次の項目を足します。既存の項目は残してください。
変数の既定値はmacOSの場合なので、同じ書き方が両方のホストで使えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.27.0
    user: "${GCSM_UID:-10001}"
    environment:
      GCS_MIMIC_SYNC_DIR: /sync
    volumes:
      - ${GCSM_DATA_DIR:-gcs-data}:/var/lib/gcs-mimic
      - ./sync:/sync
```

起動して、オブジェクトを書き、ファイルを読みます。Linuxでは`export`の行を実行し、macOSでは省きます。

```sh
mkdir -p sync data
export GCSM_UID=$(id -u) GCSM_DATA_DIR=./data
docker compose up --wait
curl --fail -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=local-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=docs%2Fhello.txt'
docker compose exec gcs gcs-mimic sync wait --endpoint http://127.0.0.1:4443
cat sync/demo-bucket/docs/hello.txt
```

このリポジトリをcloneしてある場合は、[`compose.sync.yaml`](../../compose.sync.yaml)に`compose.yaml`向けの同じ項目があります。
ほかの文書と同じく、公開イメージを使います。

```sh
export GCSM_IMAGE=ghcr.io/shiroemons/gcs-mimic:0.27.0
export COMPOSE_FILE=compose.yaml:compose.sync.yaml
docker compose up --wait --no-build
```

`docker compose up --wait`は、最初の照合でディレクトリにあるファイルを取り込み、既存のオブジェクトをすべて書き出した後に戻ります。
フラグやヘルスチェックの上書きは要りません。`depends_on`の`condition: service_healthy`でこのサーバーを待つサービスも、その後に始まります。
Linuxで`sync_dir must be an existing writable directory`と出て止まるときは、`./sync`を所有しないユーザーで動いています。先に`GCSM_UID`を設定してください。

fake-gcs-serverとの違いは次のとおりです。

- **ファイルは応答の後に書かれます。** fake-gcs-serverは応答の前にファイルを書きますが、
  gcs-mimicは先に応答するので、少しの間ファイルが無いことがあります。
  読む前に`gcs-mimic sync wait --endpoint …`で待ってください。反映できる変更を反映し終えると戻ります。
  手でファイルを置いたり編集したりした後も、同じコマンドでサーバーが取り込むまで待てます。
- **置いたり編集したりしたファイルはオブジェクトになります。** 再起動は要りません。
  fake-gcs-serverのmacOSでの調査では、メタデータを持たない普通のファイルは404でした
  （[比較（英語）](../en/fake-gcs-server-storage.md#external-edits-do-not-reconcile-metadata)）。
- **`:`を含む名前はそのまま使えます。** `logs/2026-10-28T12:00:00.log`は、ファイルシステムが`:`を保存できれば同じ名前のファイルになります。
  保存できるかは起動時にサーバーが確かめます。macOSのFinderは`:`を`/`と表示しますが、ディスク上の名前は`:`のままです。
  `\`を含む名前はファイルに書き出しません。APIからは使えます。
- **resetとreseedとpurgeは拒否されます。** `sync_dir`を設定している間、同期の対象のバケットでは、外部のファイルをリセットや消去しないためです。
  やり直すときは、データとディレクトリを、ドットファイルも含めて一緒に消します。マーカーも消えます。

  ```sh
  docker compose down --volumes
  rm -r sync data
  mkdir sync data
  ```

  データだけを消してディレクトリを残す、または`sync/*`で空にする（ドットファイルには一致しません）と、古いマーカーが残り、
  古いデータのものなのでサーバーはそのディレクトリを拒否します（`ForeignMarker`）。
- **初期ファイル（seed）は起動のたびに勝ちます。** `/data`も初期ファイルのディレクトリとしてマウントしている場合、
  その名前のオブジェクトは起動のたびに置き換わります。同期ディレクトリやAPIで変更しても、再起動を越えて残りません。
- **ライフサイクルのルールは既定では動きません。** ホストのファイルを消してしまうためです。
  動かすには`GCS_MIMIC_LIFECYCLE_EXECUTION=enabled`を設定します。
- **削除は、ガードに達するまで確認なしで反映されます。** ファイルを消すとオブジェクトも消えます。
  サーバーを止めている間に消したファイルも、次の起動で削除として扱われます。
  追跡中のファイルのうち100件以上で、かつ全体の半数を超える数が一度に消えるときだけ、サーバーは承認を待ちます。
  バケットが複数あると、1つのバケットのディレクトリを消しても、たいてい半数に届かずにそのまま反映されます。
  ガードが削除を止めたときも、APIは使えたままです。`pending_deletion_revision`のrevisionを取り出し、
  一覧を確認してから承認します（`DELETION_REVISION`は自分の値に置き換えます）。

  ```sh
  docker compose exec gcs gcs-mimic sync status --endpoint http://127.0.0.1:4443
  docker compose exec gcs gcs-mimic sync status --view deletions \
    --endpoint http://127.0.0.1:4443 --revision DELETION_REVISION
  docker compose exec gcs gcs-mimic sync approve \
    --endpoint http://127.0.0.1:4443 --deletion-set DELETION_REVISION
  ```

  ほかのオプションは[同期CLIの使い方](sync-cli.md)にあります。
- **ディレクトリはデータと対にします。** 別のデータに属していたディレクトリは、`docker compose down --volumes`の後などに`ForeignMarker`で拒否します。
  上のやり直しの手順に従ってください。

競合・復旧・残る制限は[リファレンス](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)にあります。

## 設定の対応表

0.18.0以降は、移行元と意味が変わるフラグを起動前にまとめて拒否します。
受け付けた読み替えは、nativeの書き方を1つのinfo診断に表示します。
古いフラグを残す場合は[全フラグの分類](fake-gcs-server-configuration.md#0180のフラグ分類)を確認してください。
`--port-http`には`--scheme both`が必要です。

| fake-gcs-server | gcs-mimic |
| --- | --- |
| `-scheme http`、`FAKE_GCS_SCHEME` | `--scheme http`、`GCS_MIMIC_SCHEME: http` |
| `-port 9000`、`FAKE_GCS_PORT` | `--port 9000`、`GCS_MIMIC_LISTEN: 0.0.0.0:9000` |
| `-port-http 8000`、`FAKE_GCS_PORT_HTTP` | `--port-http 8000`、`GCS_MIMIC_LISTEN_HTTP: 0.0.0.0:8000` |
| `-host 0.0.0.0 -port 4443` | 不要。イメージが最初から待ち受けます |
| `-external-url`、`-public-host` | 不要。返すリンクは各リクエストの接続先に従います |
| `-backend filesystem -filesystem-root /storage` | `/var/lib/gcs-mimic`の名前付きボリューム |
| `-backend memory` | `GCS_MIMIC_BACKEND: memory` |
| `-data /data` | 不要。マウントはそのままです |
| `-cors-headers` | 不要。ルールが必要なバケットに`cors`を設定します |

## もっと詳しく

- [よく使う設定](container-usage.md#よく使う設定)
- [詳しい移行手順](fake-gcs-server-migration.md)と[移行で困ったとき](fake-gcs-server-migration.md#移行で困ったとき)
- [GitHubで確認した利用例と移行時の違い](fake-gcs-server-migration.md#githubで確認した利用例と移行時の違い)
- [設定リファレンス](fake-gcs-server-configuration.md)
