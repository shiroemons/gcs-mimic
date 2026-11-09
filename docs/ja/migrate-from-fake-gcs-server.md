# fake-gcs-serverから移行する

[English](../en/migrate-from-fake-gcs-server.md)

gcs-mimicは普段の使い勝手を[fake-gcs-server](https://github.com/fsouza/fake-gcs-server)から受け継いでいるので、
多くのコンテナ構成は数行の変更で移行できます。
移行する価値があるかどうかを決めるのは「できなくなること」なので、このページはそこから始めます。
対象はgcs-mimic 0.14.0とfake-gcs-server 1.56.1です。

gcs-mimicは、すべての構成でそのまま置き換えられるものではありません。

## できなくなること

影響する構成が多い順に、おおまかに並べています。

| 今していること | 移行後 | 代わりにすること |
| --- | --- | --- |
| `/storage`ボリュームにオブジェクトを保存している | 形式が異なるためボリュームは再利用できず、`-filesystem-root`は無視されます。gcs-mimicは空の状態で起動します。 | [オブジェクトの中身をコピー](#既存のオブジェクトを持ってくる)するか、テスト用のデータから入れ直します。 |
| `FAKE_GCS_*`環境変数を設定している | 0.14.0より後のバージョンは読みません。警告も出ません。`FAKE_GCS_SCHEME=http`を残すとHTTPSのまま起動し、HTTPクライアントが接続に失敗します。 | [下の表](#設定の対応表)の対応するフラグか`GCS_MIMIC_*`環境変数を使います。 |
| 既存オブジェクトの世代・カスタムメタデータ・Content-Typeに依存している | 現時点では引き継げません。`gcs-mimic import fake-gcs-server`はありますが、0.14.0ではfake-gcs-serverのAPIでアップロードしたオブジェクトすべてで止まります。これらに自動で付く`projectOwner-test-project`というACLを受け付けないためです。 | テストが依存するオブジェクトをAPIで作り直すか、そのデータはfake-gcs-serverに残します。 |
| 保存先のディレクトリでオブジェクトをファイルとして見る・編集する | オブジェクトは中身の分からないファイルとして保存します。`/data`は読み取り専用の入力で、APIの書き込みは反映されません。 | [同期ディレクトリ](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)を追加します。オブジェクトをファイルとして書き出します。競合と復旧のケースはまだ一部のみの対応です。 |
| `docker exec … sh`、シェルのヘルスチェック、シェルのentrypointを使っている | イメージにはシェルもパッケージマネージャーもありません。 | `healthcheck:`の上書きを外します。イメージに専用のもの（`gcs-mimic healthcheck`）があります。スクリプトは別のコンテナから実行します。 |
| rootだけが読み書きできるディレクトリをマウントしている | サーバーはrootではなくUID/GID 10001で動きます。 | `/var/lib/gcs-mimic`には名前付きボリュームを使い、`/data`のファイルは全ユーザーが読めるようにします。 |
| Goのテストプロセスに`fakestorage`を組み込んでいる | プロセス内で動くサーバーはありません。[Goパッケージ（英語）](../../go/README.md)はRustのバイナリを子プロセスとして起動し、APIの一部に対応します。`Backend()`は使えません。 | この範囲で足りる場合に、このパッケージを使います。 |
| `fake-gcs-server`を決め打ちしたTestcontainersのモジュールやラッパーを使っている | ラッパーが旧実行ファイルと起動ログを前提にしているため、イメージの変更だけでは動きません。 | [汎用コンテナのアダプター（英語）](../en/slices/sdk-client-adapters.md)を使います。 |
| `-backend memory`で大量のデータを扱う | RAM保存には上限があり、既定ではオブジェクト本体で64 MiBです。超えると書き込みが503で失敗します。 | `memory_max_bytes`を増やす（[RAM保存の仕様（英語）](../en/slices/memory-backend.md)）か、ディスク保存を使います。 |
| 初期ファイルのディレクトリに`.metadata`ファイルを置いている | メタデータとしては読まず、通常のオブジェクトとして取り込みます。 | メタデータはAPIで設定します。 |
| `/_internal/reseed`や`PUT /_internal/config`を呼んでいる | 既定では無効です。 | 他から届かない待受で`GCS_MIMIC_ADMIN_ENABLED=true`を設定します。 |
| `?project=`なしでバケットを一覧する | Cloud Storageと同じく400を返します。SDKは最初からプロジェクトを送ります。 | `?project=local-project`を付けるか、`GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`を設定します。 |
| Pub/Subエミュレーターなしで`-event.*`を使う | 起動に失敗します。通知を待つテストが止まらないようにするためです。 | `PUBSUB_EMULATOR_HOST`でローカルのエミュレーターを指定します。 |
| `-public-host`のHostヘッダー照合に依存している | フラグは受け付けますが、照合はしません。 | `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`を設定します。 |
| Cloud Storageなら拒否されるリクエストを送っている | gcs-mimicはCloud Storageと同じように入力を検証するので、4xxになることがあります。 | リクエストを直します。 |
| resumableアップロードの取り消しのステータスを検査している | Googleが取り消し成功として定めている499を返します。 | 499を受け入れます。 |

## fake-gcs-serverのままがよい場合

次のどれかに当てはまるなら、今は移行しないほうがよいでしょう。

- Goのテストに`fakestorage`を組み込み、`Backend()`やプロセス内での実行に依存している。
- `/storage`ボリュームにある既存オブジェクトの世代やカスタムメタデータが必要。
- オブジェクトのファイルをその場で編集する作業に依存している。

## コンテナを入れ替える

```diff
 gcs:
-  image: fsouza/fake-gcs-server:1.56.1
+  image: ghcr.io/shiroemons/gcs-mimic:0.14.0
   command: ["-scheme", "http", "-external-url", "http://gcs:4443"]
   volumes:
     - ./data:/data
-    - gcs-storage:/storage
+    - gcs-data:/var/lib/gcs-mimic
```

`command:`のフラグと`./data:/data`のマウントは、多くがそのまま動きます。
動きが異なるフラグだけ、起動時に警告が出ます。
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

## 設定の対応表

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
