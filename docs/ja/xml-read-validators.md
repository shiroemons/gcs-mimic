# XMLオブジェクト読み取りの条件ヘッダー

EXT-XML-012とEXT-XML-017はPARTIALのままです。
XMLのGETとHEADは、認可と世代選択の後にIf-Match、If-None-Match、
If-Modified-Since、If-Unmodified-Sinceを評価します。
path-styleとvirtual-host、公開オブジェクトと保持された世代で同じ処理を使います。

RFC 9110の順序で、まずIf-Match、なければIf-Unmodified-Sinceを評価し、
不成立なら412を返します。続いてIf-None-Match、なければIf-Modified-Sinceを
評価し、キャッシュが有効なら本文のない304を返します。
日付は秒単位で比較します。不正な日付や重複した日付ヘッダーは無視し、
不正なETagの構文は400で拒否します。
304にはETag、Cache-Control、Last-Modifiedと、元の応答にあればExpires・Varyを返します。

PUT・DELETE・バケット操作・一覧は条件ヘッダーを引き続き拒否します。
If-Rangeも未対応です。Rangeフィールドが複数ある場合は、JSONとXMLの両方で
無視します。1フィールド内の多重レンジは、JSONで501、XMLで400になります。

RFCの単体テスト4件と、実際のルーティングを使ったプロセス内テスト2件が成功しました。
CSEKのキーなしHEADは304を返し、キーなしGETは条件ヘッダーを付けても拒否することを確認しました。
ソケットを使う検証はサンドボックス外での実行が必要です。
提供元での複合条件の優先順位や詳細な応答ヘッダーとの一致は、まだ確認していません。
[公式資料・実行コマンドと制約](../en/slices/xml-read-validators.md)を参照してください。
