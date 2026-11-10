# Dockerで使う

最短の起動方法は[READMEの「まず動かす」](README.md#まず動かす)です。`docker run`ひとつで組み込みの開発用証明書を使うHTTPSが起動し、イメージ名の後ろに`--scheme http`を付けるとHTTPになります。ハイフンは1つでも2つでも、`--name=value`の形でも指定できます。すべての設定は`docker run --rm ghcr.io/shiroemons/gcs-mimic:0.17.1 --help`で確認できます。イメージ`ghcr.io/shiroemons/gcs-mimic:0.17.1`は`linux/amd64`と`linux/arm64`向けに公開済みで、ログインなしでpullできます。Docker Hubでも同じdigestのイメージを`shiroemons/gcs-mimic:0.17.1`として公開しており、以下のコマンドではどちらの名前も使えます。CIではこのバージョンを明示してください。未公開のソースを試す場合は、チェックアウトでローカルイメージをビルドし、以下のコマンドのイメージ名を置き換えます。

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
  ghcr.io/shiroemons/gcs-mimic:0.17.1 --scheme http
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
イメージのようにサーバーがPID 1で動く場合、環境変数から求めた確認先が失敗すると、ヘルスチェックはComposeの`command`を含むサーバーのCLI引数で確認し直します。
それ以外の環境では、設定を環境変数やファイルで共有するか、
`healthcheck --url URL`で接続先を明示してください。
イメージは`GCS_MIMIC_LISTEN=0.0.0.0:4443`と
`GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`を環境変数で設定します。
ポートや保存先を変える場合は、この環境変数を上書きしてください。
nativeと同じく、環境変数はJSON設定より優先します。
ルートが読み取り専用でも動くよう、SQLiteが大きな文のジャーナルを書き出す先として
`SQLITE_TMPDIR=/var/lib/gcs-mimic`も設定します。読み取り専用のルートでデータディレクトリを
移す場合は`SQLITE_TMPDIR`も同じ場所にしてください。そうしないと、該当するリクエストが
503 `disk I/O error`になります。
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
`Location`、`mediaLink`、`selfLink`など、APIが返すリンクは、各リクエストのHostと接続のスキームから作ります。
呼び出し元は自分が接続したアドレスを受け取るので、ホスト側のポートが何番でも、ホストと別のコンテナが1台のサーバーを共有できます。
そのために`GCS_MIMIC_API_BASE_URL`を指定する必要はありません。
公開URLや署名URLは、引き続き受け取るクライアントから到達できるアドレスで作ります。
Go storageは署名するときに既定以外のポートを落とします。既定のpermissiveのサーバーは署名を検証しないため、`localhost:4443`向けのこうした署名URLもそのまま使えます。
署名を検証するサーバー（strictモード、または`GCS_MIMIC_SIGNED_URLS=verify`）でも、組み込みのテスト署名者が有効なら、その署名者自身の署名に限って`GCS_MIMIC_SIGNED_HOST_PORT`の既定値が`lenient`になるため、こうしたURLを受け付けます。
ほかの鍵による署名と、テスト署名者がないときの署名は既定値が`exact`で、403 `SignatureDoesNotMatch`になります。ワークツリーごとに別のポートを公開するなど既定のポートを使えず、ほかの鍵で署名する場合は`lenient`を設定します。明示した`lenient`はすべての署名者に適用されます。署名を検証しない構成で指定すると、効果がないため起動時にエラーになります（[署名URL](../en/signed-urls.md)、英語）。

すべての呼び出し元に同じアドレスを返したいときは、`GCS_MIMIC_API_BASE_URL`で固定します。
`Forwarded`と`X-Forwarded-*`は無視するので、Hostを書き換えるリバースプロキシでは指定が必要です。
XMLの仮想ホスト構成では不要です。`bucket.storage.example:4443`のような仮想バケットのホストへのリクエストには、設定したドメインと同じポート、接続のスキームでリンク（`storage.example:4443`）を返し、バケット名は含めません。
DNSにバケット名だけを登録していてドメイン自体を解決できない場合は、指定してください。
不正なHostやHostなしのリクエストには、bind先のアドレスを返します。
これは実際にbindしたアドレスで、`0.0.0.0`は`127.0.0.1`、`::`は`[::1]`に置き換え、ポート0は割り当て後のポートを使います。
イメージでは`http://127.0.0.1:4443`（HTTPSなら`https://`）で、ready通知の`api_base_url`で確認できます。
リポジトリのComposeファイルは、`GCS_MIMIC_API_BASE_URL`が環境にあるときだけ渡します。

macOSで数百の接続を同時に開くと、接続直後に`connection reset by peer`になることがあります。
macOSがlistenキューを`kern.ipc.somaxconn`（既定は128）で打ち切るためで、どのサーバーでも、Docker Desktopが公開したポートでも起き、gcs-mimicの設定では変えられません。
接続を再利用する、小分けに開く、リセットを再試行する、のいずれかで避けてください。
同じDockerネットワークのクライアントでは、確認した範囲で起きていません。
[測定結果](../en/evidence/zero-config-defaults.md#simultaneous-connections-on-macos)があります。

## どの構成を使うか

| 目的 | 構成 | 設定 |
| --- | --- | --- |
| fake-gcs-serverと同じく動かす。すべてのリクエストを許可し、署名は確認せず、ブラウザーはどのバケットも読める | 既定のpermissiveプロファイル | `GCS_MIMIC_SCHEME=http` |
| 本番に近いアクセス。非公開バケットは非公開のまま、公開バケットは`allUsers`で読め、ブラウザーは署名URLを使い、アプリのサーバーからCloud Storageへの呼び出しは変えない | 信頼済みリスナー付きのstrictモード。[strict localレシピ](../../examples/strict-local/README.md)（英語） | `GCS_MIMIC_SCHEME=http`、`GCS_MIMIC_AUTH_MODE=strict`、`GCS_MIMIC_TRUSTED_LISTEN=0.0.0.0:4444`（公開しない）と、バケットごとのポリシーとCORSルールを書いた`GCS_MIMIC_BUCKETS`。アプリがURLに署名するか、Railsのように`test-signer credentials`の鍵を使うなら`GCS_MIMIC_TEST_SIGNER=enabled`も追加 |
| strictモードをまだ使えない間だけ。署名URLは確認し、それ以外は制限しない | 署名を検証するpermissive | `GCS_MIMIC_SCHEME=http`と`GCS_MIMIC_SIGNED_URLS=verify` |

まずは既定の構成で始めてください。
本番に近い形で確認するには、strict localレシピのとおりに信頼済みリスナー付きのstrictモードを使います。
コンテナのスモークテストがこのブロックをそのままイメージで動かしており、各行の意味と、レシピが前提にしている既定値はレシピのREADMEで説明しています。
Go API、Railsアプリ、フロントエンドをつなぐ方法は[アプリ全体の例](../../examples/full-stack/README.md)（英語）にあります。
permissiveの`verify`は、strictモードをまだ使えない構成のための途中段階にすぎません。
不正な署名は拒否しますが、検証に通ったリクエストも署名のないリクエストもすべて許可するので、非公開バケットが非公開のままかは確認できません。
表の2行目と3行目の構成には、0.14.0以降が必要です。

### fake-gcs-serverとの違い（構成別）

fake-gcs-server 1.56.1との挙動の違いを、[移行手順](fake-gcs-server-migration.md)、[機能比較](feature-comparison.md)、[設定・環境変数の対応](fake-gcs-server-configuration.md)とリリースノートに記録した内容からまとめます。
「strictと同じ」は、プロファイルによって変わらない挙動です。

| 挙動 | fake-gcs-server 1.56.1 | gcs-mimic permissive（既定） | gcs-mimic strict |
| --- | --- | --- | --- |
| 認証 | なし。すべてのリクエストを許可する | なし。認証情報は無視し、すべてのリクエストを許可する | ローカルのID。bearerトークン、自己署名JWT、署名URL、HMAC署名。認証情報のないリクエストは匿名 |
| IAMとACL | ACLのメタデータ操作はあるが強制しない。バケットIAMのルートはない | 保存して返すが、強制しない | プロジェクトのロールとバケットIAMポリシーの事前定義ロールを強制する。オブジェクトACLの`READER`と`OWNER`は読み取りを許可する |
| 公開バケットと非公開バケット | 区別しない。どのオブジェクトも誰にでも返す | 区別しない。どのオブジェクトも誰にでも返す | 認証情報のないリクエストは`allUsers`の権限だけを使い、それ以外は401。`allAuthenticatedUsers`には登録済みのIDが必要 |
| 署名URLの検証 | 署名も有効期限も確認しない | 既定では確認しない。`GCS_MIMIC_SIGNED_URLS=verify`で確認するが、リクエストはすべて許可する | 常に確認する。ホスト、パス、クエリー、メソッド、署名したヘッダー、有効期限。Go storageのようにポートを落としたホストは、有効なテスト署名者自身の署名なら通る。`signed_host_port`を`lenient`にすると、すべての署名者で通る |
| CORSの既定 | すべてのルートで同じワイルドカード。`-cors-headers`で許可するリクエストヘッダーを足す | JSON APIは固定の既定。XMLと公開URLは、ルールのないバケットならどのオリジンも許可（`cors_fallback`が`allow-all`）。ルールのあるバケットはそのとおりに適用 | JSON APIは固定の既定。XMLと公開URLはバケットの`cors`ルールだけ（`cors_fallback`が`none`） |
| バケットの用意 | 初期ファイルのディレクトリ（`-data`）。最上位のディレクトリがバケットになる | strictと同じ | fake-gcs-serverと同じ`/data`の初期ファイルに加え、バケットごとにCORS、バージョニング、IAMポリシー、soft deleteを書けるバケットの文書。編集が既存バケットにも反映される`GCS_MIMIC_BUCKETS`（インライン）か、`GCS_MIMIC_BUCKETS_FILE` |
| soft delete | なし。削除や上書きで本文を解放する | strictと同じ | fake-gcs-serverと同じく既定では無効。バケットのポリシーか`GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS`で本文を残し、復元できる |
| タイムアウト | 19個の環境変数に設定はない | strictと同じ | リクエストの先頭部分の受信に30秒、リクエスト全体に180秒。本文が止まった場合とオブジェクトの転送には上限なし。アイドルの接続は120秒で閉じる。いずれも設定で変えられる |
| 接続数の上限 | 19個の環境変数に設定はない | strictと同じ | すべての待受の合計で1024接続。超えたクライアントは空きを待つ |
| オブジェクトサイズとメモリ | `CreateObject`はオブジェクト全体をメモリに読み込む。memory backendはすべての本文をプロセス内に持つ | strictと同じ | 本文は64KiBずつ流すので、メモリはオブジェクトの大きさに比例しない。オブジェクトは5TiBまで（`max_object_bytes`） |
| 再起動をまたぐresumableセッション | このリポジトリでは比較していない | strictと同じ | セッションと受け取ったバイトはデータボリュームに残り、再起動後も続けられ、7日で期限切れになる。memory backendでは失われる |
| 返すURL | `-external-url`ですべてのクライアントに同じアドレスを返す | strictと同じ | 各リクエストのHostと接続のスキームに従うので、コンテナとホストがそれぞれ自分のアドレスを受け取る。`GCS_MIMIC_API_BASE_URL`で1つに固定できる |
| 条件付きリクエスト | アップロードの世代条件だけ。読み取りの`If-Match`と`If-None-Match`は無視する | strictと同じ | オブジェクトとバケットの変更で世代とmetagenerationの条件を書き込みと一体で確認する。読み取りではETagの条件を確認する |
| `project`を省略したバケット一覧 | 使える | strictと同じ | 400 `required`。`GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`では`seed_project`を使う |
| 信頼済みリスナー | なし | 使えない。すべてのリクエストを許可するので、指定すると起動が止まる | `GCS_MIMIC_TRUSTED_LISTEN`。認証情報のないリクエストを`trusted-app@gcs-mimic.invalid`か`GCS_MIMIC_TRUSTED_PRINCIPAL`として扱う。[下の節](#信頼済みリスナーで非公開バケットを守る)を参照 |

速度や実測したメモリ使用量は比較していません。

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
  -v "$PWD/data:/data:ro" ghcr.io/shiroemons/gcs-mimic:0.17.1 --scheme http
curl 'http://localhost:4443/storage/v1/b?project=local-project'
```

Composeでは、サービスの既存の`volumes`へ`- ./data:/data:ro`を追記します。
環境変数は不要です。

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
| 違いはあるが、オブジェクトホールドや保持期間の対象 | 初期ファイル全体を反映せずに起動を失敗させ、readyを出さない |

ホールドとストレージクラスは比較しないため、内容が同じならホールド中のオブジェクトも`unchanged`です。
0.8.0以降は、初期ファイル全体を準備してから1つのトランザクションで反映します。途中で失敗すると、それまでのカタログと本文をそのまま残します。
0.7.0以前は保護対象のオブジェクトだけを飛ばして`blocked`として数え、残りを反映していました。
0.8.0以降の`blocked`は常に0です。ホールドを外すには`seed_dir`なしで起動し、保持期間なら期限まで待つか、初期ファイルを合わせてください。

初期ファイルのないオブジェクトには触れず、初期ファイルを消してもオブジェクトは削除しません。
APIで削除したオブジェクトは次回の起動時に再作成し、初期ファイルの編集も次回の起動時に反映します。
初期ファイルのディレクトリは監視しないため、コンテナを再起動してください。
0.8.0以降は、`--admin-enabled true`で管理APIを有効にすると`/_internal/reseed`でも反映できます（[一括反映とreseed（英語）](../en/slices/atomic-seed-reseed.md)）。
設定した上限を超えるツリー、取り込めない名前を含むツリー、コンテナのユーザー（uid 10001）が読めないファイルを含むツリーのreseedは400を返し、問題の種類を示します。
`Retry-After: 1`付きの503を返すのは、やり直せば完了しうる場合だけです。

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
gcs-mimicはそのまま残します。また、gcs-mimicの`/_internal/reseed`は、管理APIを明示的に有効にした場合だけ使えます。
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

サーバーの外で作成・編集したファイルは、新しい世代としてhash付きで取り込みます。
0.8.0以降は、LinuxのinotifyとmacOSのFSEventsでディレクトリを[継続して監視（英語）](../en/slices/continuous-directory-observation.md)し、
再起動しなくても編集を反映します。0.7.0以前は起動時だけ取り込んでいました。
readyは最初の照合より前に出るため、同期の完了を示す合図ではありません。
ファイルを消すと、holdやretentionで保護されていないliveオブジェクトも削除します。
削除数が100件以上かつツリーの半数を超える場合は、削除ガード（`GCS_MIMIC_SYNC_DELETE_GUARD`、既定`true`）で停止し、
revisionを固定した削除一覧を[`gcs-mimic sync`](sync-cli.md)で確認して承認するまで削除しません。
分割された走査でも、直前までの自動削除数を累積してこの閾値に数えます。
APIでバケットを削除してもディレクトリは残ります。同じオブジェクトをAPIとファイルの両方で
変更した場合は、どちらも相手を上書きせず、ファイルはそのまま残ります。
`sync_ingested`・`sync_reconciled`ログイベントでは、こうしたファイルを`conflicting`として数え、
`gcs-mimic sync conflicts`で一覧・解決できます。
`logs/2026-10-28T12:00:00.log`のように`\`や`:`を含む名前と、ホストのファイル名に
対応付けられないオブジェクト名は書き出さずに飛ばします。書き出し自体は止まらず、APIからは使えます。

ディレクトリには、1つのデータディレクトリと結び付ける`.gcs-mimic-sync.json`と
`.gcs-mimic-sync-work`が作られます。作成途中の`.gcs-mimic-sync.json.staging`も含めて
`.gitignore`に追加してください。マーカーは一時ファイルへの書き込みと同期を終えてから原子的に公開します。
一時マーカーの成分名はオブジェクトのすべての階層で予約されます。
この名前を使う既存のAPIオブジェクトは保持されますが、作業ツリーには対応付けられません。
同じカタログとrootに属する完成済みの一時マーカーは、再起動時に回収します。
公開済みのマーカーが別のカタログに属する場合や壊れている場合は、従来どおり起動を拒否します。
`docker compose down --volumes`などでデータボリュームを新しくすると、古いディレクトリは
`ForeignMarker`の診断で起動時に拒否されます。古いディレクトリは退避し、新しい空のディレクトリを
使ってください。データや初期ファイルのディレクトリと重ねることはできません。
同期はLinuxとmacOSで動作し、Windowsでは拒否します。
0.8.0以降はstrictでも使え、起動にはローカルのID登録が、同期の操作にはプロジェクト所有者の権限が必要です。
ディレクトリのメタデータ・所有者・拡張属性が変わると、読み取り・検索権限を検査し直します。
アクセスできない間は同期のreadinessを下げ、権限を直して観測処理が復旧するまで戻しません。

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
  ghcr.io/shiroemons/gcs-mimic:0.17.1
```

検証済みの範囲、復旧の挙動、未実装の作業は[同期の契約（英語）](../en/slices/directory-synchronization.md)を参照してください。

## よく使う設定

| 設定 | 用途 |
| --- | --- |
| `GCS_MIMIC_SCHEME` | `https`（既定）、`http`、`both` |
| `GCS_MIMIC_AUTH_MODE` | `permissive`（既定）はfake-gcs-serverと同じくすべてのリクエストを受け付ける。`strict`はローカルのIDに対してIAMとACLで各リクエストを認可する。[strict認証（英語）](../en/getting-started.md#strict-local-authentication) |
| `GCS_MIMIC_BUCKETS` | 起動時に作るバケットを、`cors`や`iamPolicy`などの設定付きでインラインのJSONに書く。`GCS_MIMIC_BUCKETS_FILE`は同じ文書をファイルから読む。[初期バケット設定](#初期バケット設定ファイルを使う) |
| `GCS_MIMIC_IDENTITIES` | strictで使うローカルのID・トークン・ロールをインラインのJSONに書く。`GCS_MIMIC_IDENTITY_FILE`は同じ文書をファイルから読む |
| `GCS_MIMIC_TRUSTED_LISTEN` | strict用。公開しない待受（例: `0.0.0.0:4444`）で、サーバーからの認証情報のないリクエストを1つのIDとして扱う。[信頼済みリスナー](#信頼済みリスナーで非公開バケットを守る) |
| `GCS_MIMIC_TEST_SIGNER` | `enabled`で、秘密鍵を公開している組み込みの署名者`test-signer@gcs-mimic.invalid`を有効にする。署名付きURLに鍵の準備がいらない。[テスト署名者（英語）](../en/signed-urls.md#built-in-test-signer) |
| `GCS_MIMIC_SIGNED_URLS` | `ignore`（permissiveの既定）は署名付きURLを検査せずに受け付ける。`verify`は署名と期限を検査する。[署名の検証（英語）](../en/signed-urls.md#verifying-signed-requests-in-permissive-mode) |
| `GCS_MIMIC_CORS_FALLBACK` | CORSルールのないバケットの公開URL・XML URLのCORS。`allow-all`（permissiveの既定）はどのオリジンのページからも読める。`none`（strictの既定）はCORSヘッダーを返さない |
| `GCS_MIMIC_API_BASE_URL` | エミュレーターが返すリンクのアドレスを固定する。未指定なら各リクエストの接続先を使う。`Host`を書き換えるリバースプロキシの後ろに置くときだけ指定する |
| `GCS_MIMIC_GRPC_LISTEN` | 任意のgRPC待受。例: `0.0.0.0:4444` |
| `GCS_MIMIC_LOG_LEVEL` | `debug`・`info`（既定）・`warn`・`error` |

`GCS_MIMIC_BUCKETS`・`GCS_MIMIC_IDENTITIES`・`GCS_MIMIC_TRUSTED_LISTEN`・`GCS_MIMIC_TEST_SIGNER`・`GCS_MIMIC_SIGNED_URLS`には、0.14.0以降が必要です。
0.13.0までは、`GCS_MIMIC_CORS_FALLBACK`の既定はどちらのモードでも`none`です。
0.11.0までのリリースは返すリンクのアドレスを1つに固定するので、`GCS_MIMIC_API_BASE_URL`も指定してください。
`docker run --rm ghcr.io/shiroemons/gcs-mimic:0.17.1 --help`ですべての設定を確認できます。

## バケットごとに設定する

待受アドレス、HTTP/HTTPS、保存先、通知するAPI URLはサーバー全体の設定です。
Composeの環境変数かCLIで指定します。設定名の対応は
[設定・環境変数](fake-gcs-server-configuration.md)を参照してください。

`GCS_MIMIC_*`の空の値は未設定とは扱わず、ほとんどの設定は起動時にエラーになります。
変数の展開に失敗したときに、permissive認証のような既定値へ黙って戻らないようにするためです。
Composeの`${VAR:-}`は空文字を渡します。
シェルや`.env`で指定したときだけ渡すには、このリポジトリの`compose.yaml`のように、値を書かずに名前だけを書きます。

```yaml
    environment:
      GCS_MIMIC_API_BASE_URL:
```

空の値に意味がある設定もあります。
`GCS_MIMIC_LOCAL_CREATOR_EMAIL`はローカルの作成者の記録を無効にし、`GCS_MIMIC_DEFAULT_LOCATION`は`US`を使います。
`GCS_MIMIC_ADMIN_ALLOWED_HOSTS`と`GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS`は一覧が空になり、`GCS_MIMIC_PUBSUB_EMULATOR_HOST`、`GCS_MIMIC_EVENT_PUBSUB_PROJECT_ID`、`GCS_MIMIC_EVENT_PUBSUB_TOPIC`、`GCS_MIMIC_EVENT_BUCKET`、`GCS_MIMIC_EVENT_OBJECT_PREFIX`は何も設定しません。

リクエスト本文を宣言したまま送信を止めたクライアントは、何も記録されないまま接続を使い続けます。
`GCS_MIMIC_BODY_INACTIVITY_TIMEOUT_MS`の既定値0では上限がなく、期限切れもログも起きないためです。
該当するクライアントを見つけるには、例えば`30000`を設定します。その時間本文が届かないリクエストは
JSONでは408 `uploadBrokenConnection`、XMLでは400 `RequestTimeout`で失敗し、サーバーは`connection_timeout`の警告を記録します。

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
ルールのないバケットについては、permissiveモードでは`GCS_MIMIC_CORS_FALLBACK`の既定値が`allow-all`で、fake-gcs-serverと同じくXML・公開URLをどのオリジンからでも読めます。
サーバーは、こうしたリクエストをバケットごとに最初の1回だけinfoレベルの`cors_fallback_default`としてログに出します。
strictモードでは既定値が`none`で、Cloud Storageと同じく、ブラウザーにはバケットの`cors`ルールが必要です。
ルールのあるバケットは、常にそのルールに従います（[CORSフォールバックの仕様](../en/slices/cors-fallback.md)、英語）。

既定では、署名を検証せずに署名付きURLを受け付けます。permissiveのまま、変更・期限切れ・未知の鍵の署名付きリクエストを拒否するには、
`GCS_MIMIC_SIGNED_URLS=verify`を追加します（[permissiveでの署名検証](../en/signed-urls.md#verifying-signed-requests-in-permissive-mode)、英語）。
この設定は`Authorization`ヘッダーの署名とPOSTポリシーも対象にするため、適当なHMACキーで署名するS3互換クライアントは拒否されます。
この構成では組み込みのテスト署名者を受け付けます。イメージにはリポジトリが含まれないため、公開済みの鍵はイメージ自身がサービスアカウントの鍵ファイルとして出力します。

```sh
docker run --rm "${GCS_MIMIC_IMAGE:-gcs-mimic:dev}" test-signer credentials > test-signer.json
```

秘密鍵は公開されているため、ローカルのテストだけで使います（[組み込みのテスト署名者](../en/signed-urls.md#built-in-test-signer)、英語）。

## 初期バケット設定ファイルを使う

ソースから実行する場合、同梱の`config/buckets.json`を読み取り専用で渡せます。
CORS・バージョニング・projectが異なる2バケットを作る例です。

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.buckets.yaml up -d --build --wait
curl --fail http://localhost:4443/storage/v1/b/initialized-assets
```

別のComposeでは、[strict localレシピ](../../examples/strict-local/README.md)（英語）のように文書をサービスのenvironmentへ`GCS_MIMIC_BUCKETS`として直接書くか、
既存のenvironmentへ`GCS_MIMIC_BUCKETS_FILE: /buckets.json`、volumesへ`./buckets.json:/buckets.json:ro`を追記します。2つの設定は同時に使えません。
Composeはインラインの値の`${VAR}`を展開するので、`$`そのものは`$$`と書きます。マウントするファイルは起動前に作ります。

```json
{"version":1,"project":"local-project","buckets":[
  {"name":"web-assets","cors":[{"origin":["http://localhost:3000"],"method":["GET"]}]},
  {"name":"private-uploads","versioning":{"enabled":true}}
]}
```

毎回、全件を検証してからseedより先に未作成バケットだけを作ります。
バケットファイルでは以後の変更はAPIで行い、再起動時に既存設定を上書きしません。項目を消してもバケットは削除しません。
APIで削除したバケットは次回起動時に再作成します。既存projectが異なる場合は
所有者を保ち、stderrで警告し、readyの`project_mismatches`へ件数を出します。

新しいバケットには、項目に
`"iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}`
と書いてIAMポリシーを付けられます。`setIamPolicy`と同じく既定のポリシーを置き換えるので、
既定のlegacyバインディングが必要なら一緒に書きます。公開アクセス防止が公開メンバーを
拒否する場合は、何も変更せずに起動を止めます。

バケットファイルでバケットの作成後に項目を編集すると、次回起動はバケットを保ったまま、まだ異なる
フィールドをstderrで警告し、readyの`unapplied`へ件数を出します。Composeを再起動しても
残るボリュームなどで既存バケットへ反映するには、`GCS_MIMIC_BUCKETS_FILE_MODE: reconcile`
を指定します。各起動は項目が書いたフィールドのうち値が異なるものだけを書き込み、変更した
バケット数をreadyの`reconciled`へ出します。項目が書かないフィールドはAPIでの変更を保ち、
バケットのロケーションは変わりません。

項目からフィールドを消しても、どちらのモードでもバケットは元に戻りません。`allUsers`の
`iamPolicy`を消してもバケットは公開のままで、次回起動で一度だけ`iamPolicy`を書かなくなった
ことを警告します。望む値、たとえば`"iamPolicy":{"bindings":[]}`を書いてください。
0.13.0から更新した最初の起動でも、項目と異なる既存バケットごとに一度だけ警告します。

選択順は`--buckets-file PATH`、`GCS_MIMIC_BUCKETS_FILE`、サーバーJSONの
`buckets_file`です。インラインの文書も`--buckets`、`GCS_MIMIC_BUCKETS`、サーバーJSONの`buckets`の順に選びます。
`buckets_file_mode`（`--buckets-file-mode`、`GCS_MIMIC_BUCKETS_FILE_MODE`）も同じ順で選び、
どちらの文書もないのに指定すると起動を止めます。既定値はインラインの`GCS_MIMIC_BUCKETS`では`reconcile`、バケットファイルでは`create`です。
インラインの値を編集すると、Composeは`docker compose up -d`だけでコンテナを作り直すので、編集がそのまま既存バケットに届きます。
ready通知は、どちらの形でも`buckets_file`として結果を返します。エントリの`project`、ファイルの`project`、`seed_project`
（既定`local-project`）の順に所有projectを決めます。
strictでもローカル管理として初期化し、項目が書いたポリシー以外のAPI権限は付与しません。
ACL、lifecycle、UBLA、`iam`、`bindings`以外のポリシー項目、未知フィールドは
空・無効でも拒否します。
0.8.0以降は`softDeletePolicy`を指定できます。既定ではsoft deleteが無効なので、使うバケットで`retentionDurationSeconds`に1秒以上を指定します。
JSON APIの固定CORSとXMLのバケット別CORSは別です。
各項目の上限は16MiB、ファイルの上限は1GiBです。`max_buckets_per_batch`を指定すると、バケット数はその値までになり、ファイルの上限は許可したバケットあたり1KiB（最低32MiB）になります。
JSON-BUCKETS-002、JSON-BUCKETS-008、DESIGN-062はPARTIAL、
BEHAVIOR-BUCKET-009はIMPLEMENTEDを維持します。
詳細は[英語の契約](../en/slices/bucket-initialization.md)を参照してください。

## 信頼済みリスナーで非公開バケットを守る

strictモードでは、認証情報のないリクエストは匿名になり、`allUsers`の権限でだけ許可されます。
ブラウザーにはこれが望ましく、ページは公開バケットを読み、非公開バケットには署名URLでだけアクセスします。
一方、アプリのサーバーもエミュレーターには認証情報を送らないことが多く（`STORAGE_EMULATOR_HOST`を設定したGo storageや、resumableアップロードを開始するHTTPクライアントなど）、同じく匿名になります。
信頼済みリスナーはこうしたサーバー専用のポートを用意し、サーバーからの呼び出しを変えずに済むようにします。
ブラウザーに渡すURLは確認の対象のままなので、エミュレーターのときだけ署名を省いているアプリには[変更が必要です](../../examples/strict-local/README.md#what-the-trusted-listener-does-not-cover)（英語）。

- 信頼済みリスナーで認証情報のないリクエストは、1つのID（信頼済みプリンシパル）として扱います。`trusted_principal`でほかのIDを指定しない限り、組み込みの`trusted-app@gcs-mimic.invalid`です。
  そのうえで、そのIDの認証情報を送ったリクエストと同じIAM・ACL・公開アクセス防止の確認を受け、書き込んだオブジェクトの所有者もそのIDになります。
- 認証情報（bearerトークン、自己署名JWT、署名URLのパラメーター、署名の`Authorization`ヘッダー、POSTポリシー）付きのリクエストは、メインのリスナーとまったく同じく認証します。
  不正な認証情報はエラーになり、信頼済みプリンシパルには切り替わりません。
  [Rails Active Storageのレシピ](rails-active-storage.md)は常に自分のトークンを送るので、サーバーがその署名者を知っている必要があります。`GCS_MIMIC_TEST_SIGNER=enabled`の組み込みテスト署名者か、IDの文書に登録した鍵を使います。
- `Origin`ヘッダーがある、または`Sec-Fetch-Site`が`none`以外のリクエストは、ルーティングの前に403になります。
  サーバー側のクライアントはどちらのヘッダーも送りません。
- リクエストのHostは、IPリテラル（`127.0.0.1:4444`、`[::1]:4444`）、`localhost`または`.localhost`で終わる名前、Composeのサービス名`gcs`のようなドットのない名前、`trusted_allowed_hosts`に書いた名前のいずれかでなければなりません。
  それ以外のHostやHostのないリクエストは、認証情報の有無にかかわらず421 `misdirected`になります。
  これはDNSリバインディングを防ぐためです。`http://evil.example`のページは自分の名前をこのリスナーに向けられ、そのときの同一オリジンのGETには`Origin`が付かず、ループバック以外への平文HTTPなら`Sec-Fetch-Site`も付きません。それでもHostは`evil.example`のままです。
  リバインディングには攻撃者のDNSが答える名前、つまりドットを含む公開の名前が必要です。攻撃者が持つオリジンについて、ページがブラウザーにIPリテラル、`localhost`の名前（ブラウザーが自分で解決します）、ドットのない名前を送らせることはできません。
- 信頼済みリスナーのすべての応答に`Cross-Origin-Resource-Policy: same-origin`と`X-Content-Type-Options: nosniff`を付けるため、別オリジンのページは画像やスクリプトとして埋め込むこともできません。
  ブラウザー以外のクライアントはどちらも無視します。
- メインのリスナーのすべてのルート（JSON、XML、アップロード、batch、`/_internal`）に応答します。
  管理ルートには引き続き`admin_principal`が必要なので、トークンなしで使えるのは`trusted_principal`がそのIDを指すときだけです。
  gRPCには信頼済みリスナーはありません。
- 非公開のコンテナネットワーク向けなので、`scheme`に関係なく平文HTTPで待ち受けます。
  `max_connections`とすべてのタイムアウトは、ほかのリスナーと共有します。
  `GCS_MIMIC_API_BASE_URL`でアドレスを固定しない限り、APIが返すリンクは各リクエストのHostに従うため、`gcs:4444`経由で開始したresumableセッションはそのまま`gcs:4444`で続けられます。

`GCS_MIMIC_TRUSTED_LISTEN`（`--trusted-listen`、JSONの`trusted_listen`）を設定すると開きます。
`GCS_MIMIC_TRUSTED_PRINCIPAL`（`--trusted-principal`、JSONの`trusted_principal`）を省略すると、信頼済みプリンシパルは組み込みの`trusted-app@gcs-mimic.invalid`です。
このIDは、バケットの文書（`GCS_MIMIC_BUCKETS`か`GCS_MIMIC_BUCKETS_FILE`）が作るすべてのバケットのproject（項目の`project`、なければ文書の`project`）と、実際に使う`seed_project`で`roles/storage.admin`を持ちます。アプリのサーバーはプロジェクト所有者のサービスアカウントと同じ操作ができ、strictモードはIDの文書なしで起動できます。
このロールは暗黙の付与なので、ディレクトリ同期の制御に必要な明示的な`roles/storage.admin`にはなりません。IDの文書でそのロールを付与するまで、信頼済みリスナーからの同期の制御は403になります。
IDの文書（`GCS_MIMIC_IDENTITIES`か`GCS_MIMIC_IDENTITY_FILE`）に`trusted-app@gcs-mimic.invalid`の項目を書くと、そのロールに置き換わります。
ほかのIDとして扱うには、`GCS_MIMIC_TRUSTED_PRINCIPAL`にIDの文書のIDか、有効にした組み込みテスト署名者を指定します。
待受なしでのプリンシパルの指定、permissiveモード（すべてのリクエストがすでに許可されるため）、`unix_socket`との併用、ほかのリスナーと同じアドレス、IDの文書にも有効な組み込みテスト署名者にも当たらないプリンシパルの指定のいずれかでは、起動が止まります。
ready通知は、bindした`trusted_listen`と、実際に使う`trusted_principal`を返します。

Composeのネットワークエイリアス`storage.compose.internal`やKubernetesのサービス名など、ドットを含む名前でアプリが接続する場合は、その名前を`GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`（`--trusted-allowed-hosts`、JSONの`trusted_allowed_hosts`）に書きます。
ポートやスキームを含まないDNS名をカンマで区切って最大32件指定でき、大文字小文字と末尾のドットは区別しません。`trusted_listen`が必要です。
書いた名前はほかの受け付けるHostと同じく通るので、公開DNSで他人が答えられない名前だけを書いてください。

> [!WARNING]
> 信頼済みリスナーのポートは公開せず、ほかの人やマシンと共有するネットワークでbindしないでください。
> 受け付けるHost（IPアドレスなど）で届いたリクエストは、認証情報なしで`trusted_principal`として扱われます。
> ブラウザーとHostの確認はWebページを防ぐためのもので、ほかのプログラムは防げません。

ワイルドカードのアドレスでも警告は出しません。
コンテナ内では、ほかのコンテナから届くように`0.0.0.0`でbindする必要があり、警告は起動のたびに出てしまうからです。
`ports:`に載せないことが保護になります。

[strict localレシピ](../../examples/strict-local/README.md)（英語）が完全な例です。
アプリのcompose.yamlに貼り付ける`gcs`サービスで、アプリのコンテナ向けの信頼済みリスナーと組み込みのID、ブラウザー向けに公開したメインのリスナー、environmentに直接書いたバケット、署名URL用の組み込みテスト署名者がそろっています。
[アプリ全体の例](../../examples/full-stack/README.md)（英語）では、Go API、Railsアプリ、フロントエンドをこれにつなぎます。
コンテナのスモークテストがどちらもイメージで動かしているので、ここの設定を自分で組み合わせず、これらを出発点にしてください。

`gcs`はドットのない名前なので、レシピでは`trusted_allowed_hosts`は不要です。
レシピのRails向けの構成が付けるエイリアス`gcs.localhost`も、`.localhost`の名前なので不要です。
ドットを含むエイリアスを使う場合は、サービスにネットワークエイリアスを付けて、その名前を書きます。

```yaml
services:
  gcs:
    environment:
      GCS_MIMIC_TRUSTED_ALLOWED_HOSTS: storage.compose.internal
    networks:
      default:
        aliases:
          - storage.compose.internal
  app:
    environment:
      STORAGE_EMULATOR_HOST: storage.compose.internal:4444
```

アプリはトークンを送らないので、組み込みのIDにトークンはありません。
アプリに必要なロールだけを与えるには、レシピのREADMEのように、IDの文書に`trusted-app@gcs-mimic.invalid`をそのロールで書きます。
ページは`http://localhost:${GCS_MIMIC_PORT:-4443}`を使います。
ポリシーで`allUsers`に`roles/storage.objectViewer`を与えたバケットは匿名で読め、非公開のオブジェクトは署名URLがなければ401になります。
アプリはこの署名URLを、`gcs:4444`ではなくブラウザーが使うアドレスに対して、IDの文書に登録した鍵か組み込みテスト署名者で作ります（[署名URL](../en/signed-urls.md)、英語）。
イメージのhealthcheckはメインのリスナーを確認するので、変更は不要です。
DESIGN-101はPARTIALです。[検証結果（英語）](../en/evidence/trusted-listener.md)を参照してください。

## 詳しい手順

- [HTTPSと証明書](../en/local-https.md#local-https-with-an-explicit-development-ca)
- [SDK・認証・ブラウザー](../en/getting-started.md)
- [ソースからのビルドと検証](development.md)
- [イメージ配布と対応環境](../en/container-distribution.md)
- [今後の導入機能](roadmap.md)
