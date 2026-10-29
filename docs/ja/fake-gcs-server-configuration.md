# 設定・環境変数の対応

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

fake-gcs-serverの設定は、名前を一括置換せず用途ごとに対応させます。
gcs-mimicは既知19個の`FAKE_GCS_*`を移行案内付きのエラーにし、未知の名前は警告します。
空の値もエラーです。別名としては使えません。

[English](../en/fake-gcs-server-configuration.md) ·
[実装状態の原本](../../compatibility/features.json)

## gRPCの待受

gRPCは既定で無効です。`--grpc-listen 127.0.0.1:4444`、環境変数
`GCS_MIMIC_GRPC_LISTEN=127.0.0.1:4444`、またはJSONの
`{"grpc_listen":"127.0.0.1:4444"}`で、HTTP・HTTPSとは別の平文h2cポートを開きます。
優先順位はCLI、環境変数、設定ファイルの順です。Go SDKでは
`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444`を指定します。
Storage v2のバケットCRUD・更新・IAM・保持ロック、オブジェクト入出力・一覧・削除・復元、
resumable（CancelResumableWriteによる取消を含む）・rewrite・compose・move・双方向範囲読み取りに部分対応しています。
Rapid append、未対応フィールドと未知のRPCは、修正案付きErrorInfoを含む
再試行されない`UNIMPLEMENTED`を返します。同じエンドポイントで、比較対象の旧Storage v1の12 RPC（プロジェクト省略のバケット一覧を含む）も提供します。
[契約と制限](grpc-v2-foundation.md)を参照してください。

## 並行アップロードと503

APIのアップロードに専用の本数枠はありません。`max_connections`（既定32）が同時接続数を制限し、超えた接続は待ちます。
64へ上げた場合の64本、既定32での12本・40本の同時アップロードを通信テストで確認しています。
アップロードの事前確認とresumableの作成・読込・checkpointは、既存のカタログ期限内で受付枠が空くまで待ちます。
resumableの開始から128 KiBの最終PUTまで、32接続で40本、64接続で64本が成功することも確認しています。
ほかのカタログ呼び出しは、同時32件の枠が埋まると503になります。
本文は64 KiBのチャンクと深さ2のチャネルで転送します。

停止時は`shutdown_timeout_ms`の範囲でアップロードと後始末を待ちます。
EOF前の切断では公開せず一時ファイルを削除し、カタログの停止開始後に新しく受け付けたアップロードは`catalog is closed`を含む503になります。
JSONのオブジェクト503は、安全に表示できる受付枠の飽和、カタログ待機のタイムアウト、I/O、破損の原因と対処を示します。
タイムアウトでは世代条件付きで再試行するか、`catalog_wait_timeout_ms=60000`などを検討してください。
本文の受信完了後は応答がタイムアウトしても公開が完了しうるため、条件なしで再試行する前に状態を読み直します。
内部エラーの任意の文字列や保存先のパスは公開しません。

機能IDはDESIGN-070・DESIGN-071です。GET・一覧の並行処理、停止・切断時の後始末も含む検証と制限は、
[英語の契約文書](../en/slices/migrate-upload-admission.md)に記載しています。両機能の全体の状態は引き続きPARTIALです。

## fake-gcs-serverの19個の環境変数

調査対象はfake-gcs-server`v1.56.1`、コミット
`896ece51e49eed0946089f3089f6343855c0d106`です。
2026-10-24に確認した実行ファイルの設定を示します。
任意のCLIフラグから環境変数名を作る仕組みではありません。
Goライブラリのコンストラクターは、この設定層を自動利用しません。

| 環境変数 | CLI | 既定値 |
| --- | --- | --- |
| `FAKE_GCS_PORT` | `-port` | `4443` |
| `FAKE_GCS_PORT_HTTP` | `-port-http` | `8000` |
| `FAKE_GCS_BACKEND` | `-backend` | `filesystem` |
| `FAKE_GCS_FILESYSTEM_ROOT` | `-filesystem-root` | `/storage` |
| `FAKE_GCS_PUBLIC_HOST` | `-public-host` | `storage.googleapis.com` |
| `FAKE_GCS_EXTERNAL_URL` | `-external-url` | スキーム・ホスト・ポートから生成 |
| `FAKE_GCS_SCHEME` | `-scheme` | `https` |
| `FAKE_GCS_HOST` | `-host` | `0.0.0.0` |
| `FAKE_GCS_DATA` | `-data` | 空 |
| `FAKE_GCS_CORS_HEADERS` | `-cors-headers` | 追加ヘッダーなし |
| `FAKE_GCS_EVENT_PUBSUB_PROJECT_ID` | `-event.pubsub-project-id` | 空 |
| `FAKE_GCS_EVENT_PUBSUB_TOPIC` | `-event.pubsub-topic` | 空 |
| `FAKE_GCS_EVENT_BUCKET` | `-event.bucket` | 絞り込みなし |
| `FAKE_GCS_EVENT_OBJECT_PREFIX` | `-event.object-prefix` | 絞り込みなし |
| `FAKE_GCS_EVENT_LIST` | `-event.list` | `finalize` |
| `FAKE_GCS_LOCATION` | `-location` | `US-CENTRAL1` |
| `FAKE_GCS_CERT_LOCATION` | `-cert-location` | 空・組み込み証明書 |
| `FAKE_GCS_PRIVATE_KEY_LOCATION` | `-private-key-location` | 空・組み込み鍵 |
| `FAKE_GCS_LOG_LEVEL` | `-log-level` | `info` |

HTTPだけで起動する`-scheme http`でも、既定のポートは`4443`です。
`8000`は`-scheme both`で追加されるHTTPリスナー用です。
両方を使う場合、自動生成されるexternal URLはHTTPSを選びます。

公式イメージのentrypointは`-data /data`を渡します。
表の空という既定値とは異なり、公式イメージでは`/data`から初期読込します。

`-public-host`は公開URLのホストと、対象オブジェクトルートのHost照合に使います。
ポートを指定した場合は照合にも含まれます。待受アドレスやアクセス権は変えません。
`-external-url`は再開可能アップロードなどのAPI応答URL用で、末尾の`/`は除かれます。

`-cors-headers`は全体で許可するリクエストヘッダーの追加です。
バケットごとのCORSや、公開するレスポンスヘッダーの指定ではありません。
`-location`は全体のバケット作成時の既定値です。
イベント設定は1つの通知先と絞り込みを設定し、バケット別ポリシーを作りません。

## 優先順位と入力エラー

fake-gcs-serverは、未設定または空の環境変数に既定値を使います。
有効なCLI値が環境変数より優先され、同じフラグを繰り返した場合は最後を使います。
公式イメージではentrypointの`-data /data`が`FAKE_GCS_DATA`より優先されます。
変える場合はCLIに`-data /another-directory`を追加します。

両方のポート環境変数はCLIより先に解析します。数値でない値は、
CLIで上書きする場合や追加HTTPリスナーを使わない場合も起動エラーになります。
解析可能な範囲外の数値なら、有効なCLI値で置き換えてから最終検証できます。
未知の`FAKE_GCS_*`名は無視します。

ログレベルは`debug`、`info`、`warning`、`warn`、`error`です。
ヘルプにあっても`trace`、`fatal`、`panic`は受け付けません。
通知を有効にするにはprojectとtopicの両方が必要です。
通知種別は`finalize`、`delete`、`metadataUpdate`、`archive`で、
ヘルプにない`archive`も使えます。通知種別は通知有効時だけ検証します。

## gcs-mimicでの対応

gcs-mimicの優先順位はCLI、環境変数、サーバーJSON設定、既定値の順です。
未知の`GCS_MIMIC_*`はエラーになり、空の値が常に既定値扱いになるわけではありません。
CLI・環境変数・JSON設定と、独立して読めるTLS・identityの不正をまとめて検査します。
stderrへ`source`・元の`key`・直し方の`fix`を持つJSONを、警告、エラーの順で出します。
エラー時は非ゼロ終了し、ready通知もデータディレクトリ作成も起きません。
値や秘密は表示せず、安全性を確認した接続先などに限って移行例へ反映します。
未対応の将来設定を利用可能とは案内しません。

`UX-003-LOG-LEVEL`では、stderrの運用ログを`event`と`level`を持つJSONに統一しています。
JSONの`log_level`、環境変数`GCS_MIMIC_LOG_LEVEL`、CLIの`--log-level`で最小レベルを指定します。
優先順位はCLI、環境変数、JSON、既定値`info`の順です。
`debug`・`info`・`warn`・`error`を受け付け、`warning`は`warn`として扱います。
`debug`には接続終了、`info`には初期読込と停止、`warn`には通信失敗・タイムアウト・後回しの後始末、
`error`にはデータディレクトリのロック解放失敗を出します。
空文字や未対応のレベルはカタログを開く前にエラーにします。
入力の警告・エラー診断と、stdoutの`ready`・`stopped`はレベルに関係なく表示します。
旧`FAKE_GCS_LOG_LEVEL`と`-log-level`は引き続きエラーにし、native設定への変更を案内します。

未知の旧変数の値は解釈せず、非UTF-8でも警告のままです。
既知変数・native設定の文字コード不正は、独立したCLIやファイルの不正とまとめて表示します。

旧フラグはハイフン1本・2本、値の分離・`=`形式を検出します。
nativeの`serve --scheme http`や`--object -data`の値はそのまま扱います。
Composeの`command`はCMDを置き換えるため、サーバー引数を渡す場合は`serve`を含めてください。
イメージの環境変数`GCS_MIMIC_LISTEN=0.0.0.0:4443`と
`GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`で待受と永続化の既定値を保ちます。
変更する場合は環境変数を上書きすると、自動ヘルスチェックも同じ待受を使います。
`GCS_MIMIC_PORT`・`GCS_MIMIC_IMAGE`・`GCS_MIMIC_TLS_DIR`はCompose呼び出し側だけに渡してください。

イメージは`GCS_MIMIC_DEFAULT_SEED_DIR=/data`を設定します。
`seed_dir`が未指定なら、存在してデータ・同期ディレクトリと重ならない`/data`を読み込み、
それ以外は何も表示せずに無視します。[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。
0.2.1以前は明示した`GCS_MIMIC_SEED_DIR`からだけ、初期ファイルを一度だけ読み込んでいました。
[0.3.0のリリースノート（英語）](../en/releases/0.3.0.md)を参照してください。
設定はCLI・環境変数・JSONを合わせて1024件、元のキーはUTF-8で256バイトまでです。
JSONの重複キーも件数へ含め、ファイル全体は64KiBまでに制限します。
[契約と検証結果](../en/evidence/configuration-diagnostics.md)を参照してください。

| 用途 | 現在の設定 | 違い |
| --- | --- | --- |
| HTTP・HTTPS | `GCS_MIMIC_SCHEME` | `http`・`https`・`both`。bothではprimaryがHTTPS |
| 待受ホスト・ポート | `GCS_MIMIC_LISTEN` | `IP:port`を1つ指定 |
| 追加のHTTP待受 | `GCS_MIMIC_LISTEN_HTTP` | both専用。既定はprimaryのIPと8000番 |
| API応答に載せる接続先 | `GCS_MIMIC_API_BASE_URL` | 公開URLの生成とは別 |
| 公開origin | `GCS_MIMIC_PUBLIC_BASE_URL` | DNS名のXML仮想ホストと、任意指定のlegacy Host照合に使用 |
| 初期ファイル | `GCS_MIMIC_SEED_DIR`、`GCS_MIMIC_DEFAULT_SEED_DIR`（イメージでは`/data`） | 起動のたびに反映する。APIでのメタデータ変更を含め、違いがあれば新しい世代にし、完全に同じオブジェクトだけ世代を残す。reseedのルートはない |
| 初期ファイルのContent-Type | `GCS_MIMIC_SEED_CONTENT_TYPE` | 既定は拡張子から決める`extension`。`octet-stream`で以前の既定値に戻す |
| 初期バケットのproject | `GCS_MIMIC_SEED_PROJECT` | 所有projectを指定 |
| 永続データ保存先 | `GCS_MIMIC_DATA_DIR` | 旧保存先の直接流用は不可 |
| 証明書 | `GCS_MIMIC_TLS_CERT` | 対応する鍵も必要 |
| 秘密鍵 | `GCS_MIMIC_TLS_KEY` | 対応する証明書も必要 |
| 公開URLの生成 | `url public --base-url` | 単体ではHost照合を設定しない |
| 仮想ホストのルート | `GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS` | 同じHost照合ではない |
| バケットCORS | JSONバケットAPI | XML用・JSONは別の固定ルール |
| ルールのないバケットのCORS（`-cors-headers`の代わり） | `GCS_MIMIC_CORS_FALLBACK=allow-all` | CORSルールのないバケットのXML・公開URLだけに適用。正しい形式のリクエストヘッダーはすべて許可。全体のCORSルールは設定できない |
| バケットlocation | JSONバケットAPI | 公式のregional・predefined dual-region名と`US`・`EU`・`ASIA`。大文字小文字を区別せず、大文字のlocationと対応するlocationTypeを返す |
| メモリ保存モード | なし | 未対応 |
| イベント通知先・絞り込み | なし | 未対応 |
| 全体のlocation既定値 | `GCS_MIMIC_DEFAULT_LOCATION` | nativeのみ。既定は`US`。JSONバケットAPIと同じ名前に対応。空文字・JSONのnullは`US` |
| ログレベル選択 | `GCS_MIMIC_LOG_LEVEL`・`--log-level`・JSONの`log_level` | 既定は`info`。`debug`・`info`・`warn`・`error`、`warning`は`warn`の別名。診断とstdoutのready/stoppedは常に表示 |

HTTPとHTTPSを同時に受け付ける場合は、次のように起動します。

```sh
target/debug/gcs-mimic serve --scheme both \
  --listen 127.0.0.1:4443 --listen-http 127.0.0.1:8000 --data-dir data
```

両方の待受でカタログ・認証・接続数上限を共有し、TLSの握手待ちも上限に含めます。
readyの`listen`はprimaryを保持し、`listeners`に実際の待受アドレスとschemeを追加します。
API originの既定はprimaryのHTTPSです。HTTPで開始したresumable uploadも同じoriginを通知します。
別の接続先を通知する場合は`--api-base-url`を指定してください。
`--listen-http`はboth以外で拒否します。同じ非ゼロの待受アドレスも拒否します。
両方をポート0にすると独立して空きポートを確保します。
どちらかのbindに失敗するとreadyを出さず、socketとカタログのlockを解放します。
終了時には両方の待受を閉じます。ComposeではHTTPのポートも明示して公開してください。
`FAKE_GCS_SCHEME`・`FAKE_GCS_PORT_HTTP`は引き続きエラーにし、上のnative設定を案内します。
対象ケース`UX-003-DUAL-LISTENER`の範囲は
[契約と検証結果](../en/slices/configuration-workflows.md#dual-listener-behavior-and-verification)に記載しています。
親機能`UX-003`は、独立した未対応ケースが残るためPARTIALのままです。

ネイティブCLIは`serve --scheme http`などの形式です。
全項目のCLI・既定値は
[英語の設定リファレンス](../en/getting-started.md#native-binary-and-configuration)、
用途別の起動例は[移行ガイド](../en/fake-gcs-server-migration.md#startup-options-and-current-equivalents)
を参照してください。

初期ファイルの読込では、元のMIME・メタデータをそのまま保持するわけではありません。
詳細は[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。

`--public-base-url`はHTTP(S)のoriginです。パスはルートだけを許可し、DNS名を小文字に、
HTTPの80番・HTTPSの443番を省略した形へ正規化します。
公開originにはポート0を指定できません。`:4443`など、接続できるポートを使ってください。
DNS名はXML仮想ホストの基底ドメインへ加え、明示リストとの合計を32個までに制限します。
同じ名前は統合しますが、明示リスト内の重複はエラーです。
IPも使えますが、そのIPからの仮想ホスト生成だけを無効にし、警告します。
ready通知の`public_base_url`は正規化した文字列、未指定時はnullです。
`public_virtual_host_enabled`はDNS名ならtrue、IPまたは未指定ならfalseを返します。
`127.1`・`0x7f.0.0.1`・`2130706433`など、ブラウザがIPv4として解釈する別表記は拒否します。
`127.0.0.1`のような正規のIPv4表記を指定してください。

nativeのpath-styleは任意のHostを受け付けます。
`--compat-profile legacy-emulator`と公開originを併用すると、上流に合わせたHost照合を有効にします。
公開authorityに一致すればパスからバケットを選び、一致しなければHost全体をバケットとして扱います。
Go SDKなどXMLで取得するクライアントではAPI接続先と公開authorityを揃えてください。
JSON・probeの予約パスは引き続き使え、明示した別のXML基底ドメインは既存のXML優先を保ちます。
Hostの入口では構文だけを検査するため、`fake_gcs`などのCompose名もJSON・probeで使えます。
XMLでは既存のlegacyバケット名規則に従い、Host全体をバケット名に使います。
[Host照合の詳細](../en/slices/configuration-workflows.md#host-matching-measured-reference-and-normative-rules)を参照してください。

`url public`と`url sign`は`--config PATH`を明示した場合だけ、そのJSONの`public_base_url`を既定に使います。
`--base-url`が優先します。環境変数や暗黙の設定ファイルは読みません。
明示したファイルの構文・64KiB上限・平坦な文字列または非負整数の型・公開originを検証し、
サーバー専用設定の意味は検証しません。TLS・identity・保存先も開きません。
ホスト・コンテナ間・ブラウザのそれぞれで到達できる接続先を指定します。
URLヘルパー、API応答用URL、権限は引き続き別です。
strict署名は最終的にクライアントが使うURLへ署名し、後からホストを書き換えません。
DESIGN-055・UX-003はPARTIALのままです。trusted proxy由来のoriginはDESIGN-056の範囲外作業です。
`--default-location`・`GCS_MIMIC_DEFAULT_LOCATION`・JSONの`default_location`で、
nativeの新規バケットのlocation既定値を選べます。既定は`US`です。
`US`・`EU`・`ASIA`と、公式一覧のregional名（`ASIA-NORTHEAST1`など）、
predefined dual-region名（`ASIA1`・`EUR4`・`EUR5`・`EUR7`・`EUR8`・`NAM4`）に対応します。
大文字小文字を区別せず、大文字のlocationと対応するlocationTypeを返します。
設定の空文字・JSONのnullは既定の`US`を使います。
優先順位はCLI、環境変数、JSON設定、既定値の順です。
APIと初期化ファイルでlocationを省略した場合や空文字・nullの場合に適用し、空でない明示値を優先します。
未知の名前は有効な値の例を含むエラーで拒否します。
configurable dual-regionの配置設定とRPOは未対応です。
固定した名前の一覧・確認日・制限は[契約](../en/slices/sdk-regional-locations.md)を参照してください。
設定を変えて再起動しても既存バケットは書き換えません。物理的な配置は再現しません。
`legacy-emulator`では明示した既定値を拒否します。
legacyの作成APIは`US-CENTRAL1`を大文字小文字を区別せず受け付けますが、
その値は保存せず、内部には`US`を保存して固定の`US-CENTRAL1`を返します。
ほかの明示locationは拒否します。
`FAKE_GCS_LOCATION`は引き続き起動エラーとし、対応する名前なら大文字に正規化して
`GCS_MIMIC_COMPAT_PROFILE=native`と`GCS_MIMIC_DEFAULT_LOCATION`を案内します。
たとえば`US-CENTRAL1`には`GCS_MIMIC_DEFAULT_LOCATION=US-CENTRAL1`、
`asia-northeast1`には`GCS_MIMIC_DEFAULT_LOCATION=ASIA-NORTHEAST1`を示します。
未知の値は表示せず、有効な名前の例を示します。legacyプロファイルの全体の既定値は未対応です。
UX-003-REMAINING-SETTINGSの範囲はこの既定値と移行案内で、親のUX-003はPARTIALです。
このスライスでは全体のCORS設定を追加していません。
後続のDESIGN-100で、CORSルールのないバケットの扱いを選ぶ`GCS_MIMIC_CORS_FALLBACK`（`none`・`allow-all`）を追加しました。
`FAKE_GCS_CORS_HEADERS`は引き続き起動エラーとし、まず削除するよう案内します。JSON APIはもともと正しい形式のヘッダーをすべて許可するためです。
ブラウザからCORSルールのないバケットのXML・公開URLを読む場合に限り、`GCS_MIMIC_CORS_FALLBACK=allow-all`も案内します。

リポジトリのComposeで使う`GCS_MIMIC_PORT`はホスト側の公開ポート用です。
サーバー自身の環境変数ではなく、バイナリへ渡しても待受設定にはなりません。

## 根拠と検証範囲

Go`1.27.3`で固定版の設定パッケージのテストと個別確認を実行しています。
19個の値と、14件の優先順位・入力検証にはentrypointの引数も含みます。
詳細は[英語の設定検証記録](../en/evidence/fake-gcs-server-configuration.md)にあります。
全上流テスト、実際のPub/Sub通知、証明書の読込までは検証していません。

不足機能の実装順は[ロードマップ](roadmap.md)で管理します。
