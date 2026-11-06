# HTTPアダプターの境界

管理ルートはsync操作も含め、Originがあるリクエストと、Sec-Fetch-Siteが
`none`以外のリクエストを403で拒否します。
公開DNSのHostは、設定したAPI・公開ホスト・virtual-hostの基底ドメインか
`admin_allowed_hosts`の項目に一致しなければ421になります。
IPリテラル、1ラベルのサービス名、予約されたローカルDNS接尾辞は許可します。
通常のcurl、Go facade、sync CLIは変更せずに使えます。
`admin_allowed_hosts`はポートを含まないDNS名をカンマで区切って最大32件指定します。
Kubernetesの短縮名（`gcs-mimic.my-namespace`、`gcs-mimic.my-namespace.svc`）や
独自のクラスタードメインは明示的な追加が必要です。
例: `--admin-allowed-hosts gcs-mimic.my-namespace.svc`。

permissiveでは認証がなく、JSON APIのCORSは任意のOriginを許可します。
ループバックだけで待ち受けても、ブラウザーで開いた悪意あるページから
ローカルのデータを読み取れます。秘密を含むfixtureにはstrictを使ってください。
バケットのCORS設定ではJSON APIのOriginを制限できません。

HTTPバッチ（BEHAVIOR-COMMON-008）は、サーバーごとに最大2本です。
要求を読み始める前に枠を確保し、応答本文の消費または切断まで保持します。
満杯の場合は本文を読まずに順番を待ち、枠が空くと処理を開始します。
HTTP接続での待機はrequest_timeout_msのハンドラー期限（既定180000ms）と
object_request_timeout_msのバッチ受付期限（既定0で無効）の早い方で打ち切り、
JSONの503 backendErrorを返します。ハンドラー期限で打ち切った場合は、
`Connection: close`を付けて接続を閉じます。
要求の10,000,000バイト上限と100呼び出しは維持します。応答はパートごとにchunkedで返し、全体の上限はありません。パートの上限は1 GiBです。
読まないクライアントは後続のパートを止め、切断すると残りを取り消します。パートのタイムアウトはそのパートだけの503で、後続のパートは動きます。これらはプロセス全体のRSS上限ではありません。
外側の認証情報は各partで上書きでき、認可はpartごとに評価します。
[英語の契約](../en/slices/http-batch.md)も参照してください。

gzip展開の同時実行数は、サーバーごとにmax_blocking_threadsの4分の1を切り上げ、
最低1に設定します。メタデータとアップロードの展開はこの枠を共有し、別のサーバーとは共有しません。
acceptの一時的な接続エラーでは待ち受けを継続し、資源不足では10msから最大1秒まで
待機を増やして再試行します。待機中も終了要求を処理します。

未知の共有リスナールートは、`error.code`、`error.message`、
`error.errors`を含むJSONの`404 notFound`を返します。
無効なXMLバケットパスも、このJSONエラー境界を維持します。
health・readinessの応答形式は従来どおりです。
`Content-Length: 00`を含む数値のゼロは、本文なしと判定します。
Transfer-Encodingや不正な長さは、本文を読み残す可能性があるものとして扱います。

無効なオブジェクトページトークンは、入力値を含まない固定のエラーを返します。
CORS lookupのI/O診断はエラー種別だけを表示し、内部のファイル名や原因文は出しません。
JSON mediaの304応答では、任意ヘッダーの欠落によるpanicを避けます。

本文バッファの拡張、Globの探索スタック、16進デコード、Originの検査、
resumable URLのソート、legacyバケット応答と接続タイマーの確保を整理しました。
性能やメモリ使用量の改善値は測定していません。

## 検証

未実施: SDK、コンテナ、provider differential。
