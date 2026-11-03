# HTTPアダプターの境界

管理ルートはsync操作も含め、Originがあるリクエストと、Sec-Fetch-Siteが
`none`以外のリクエストを403で拒否します。
公開DNSのHostは、設定したAPI・公開ホスト・virtual-hostの基底ドメインか
`admin_allowed_hosts`の項目に一致しなければ421になります。
IPリテラル、1ラベルのサービス名、予約されたローカルDNS接尾辞は許可します。
通常のcurl、Go facade、sync CLIは変更せずに使えます。
`admin_allowed_hosts`はポートを含まないDNS名をカンマで区切って最大32件指定します。

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

2026-11-03の型検査、対象の単体テスト、production lint、整形検査は成功しました。
ただし、既存のupload-frameソケットテスト1件は、サンドボックスがlocalhostのbindを
`Operation not permitted`で拒否したため失敗しています。
ソケット動作はサンドボックス外での検証が必要です。
SDK・コンテナ・実際のGoogle Cloudへの検証は実行していません。
コマンドと件数は[英語の検証記録](../en/slices/http-adapter-boundaries.md)を参照してください。
