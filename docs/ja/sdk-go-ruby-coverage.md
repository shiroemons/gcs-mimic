# Go・Ruby SDK の対応状況調査（2026-10-27）

<!-- markdownlint-configure-file {"MD013": false} -->

`main` の `461bdd9` に対して、公式の Go・Ruby Storage SDK のどの呼び出しが動くかを記録します。固定版の
SDK スイートに加えて、スイート外の SDK メソッドを呼ぶ一度きりのプローブを実行しました。これはこの時点の
証跡であり、`compatibility/features.json` の状態は変えません。英語版:
[SDK coverage (en)](../en/evidence/sdk-go-ruby-coverage.md)。

| 入力 | バージョン |
| --- | --- |
| Go | 1.27.3 |
| `cloud.google.com/go/storage` | v1.70.0 |
| Ruby / Bundler | 4.0.9 / 4.0.23 |
| `google-cloud-storage`（Ruby） | 1.62.1 |

## 方法

1. `just check-sdk`: 固定版の Go スイートを permissive と strict の HTTP サーバーで実行し、続けて Ruby の
   セルフチェック、移行シナリオ、permissive、V4 署名を含む strict を実行。
2. プローブ: 新しいデータディレクトリで permissive の HTTP サーバー（`serve --scheme http`）を 1 つ起動。
   Go は既定のクライアント（JSON API、XML 読み取り）と `WithJSONReads` のクライアントを使い、呼び出しごとに
   20 秒の期限を付けた。Ruby は `Google::Cloud::Storage.anonymous` を `retries: 0` で使った。成功した設定は
   読み戻し、受け付けて無視していないかを確かめた。

## 結果

| 実行 | 結果 |
| --- | --- |
| `just check-sdk` | PASS（exit 0）: Go vet、トップレベル 53 件とサブテスト 89 件で失敗・スキップなし。Ruby はセルフチェック、移行シナリオ、permissive、V4 署名 URL を含む strict |
| Go プローブ | 87 件: 動作 49、501 が 21（うち 8 件は期限なしでは戻らない）、404 が 16、gRPC 接続失敗 1 |
| Ruby プローブ | 101 件: 動作 56、501 が 30、404 が 15 |
| 受け付けて無視した値 | 両 SDK とも見つからなかった |

### 凡例

| 記号 | 意味 |
| --- | --- |
| ✅ | 動作する。書いた値が読み戻せる、または期待どおりのエラーが返る |
| 501 | `notImplemented` で明示的に拒否 |
| 501⏳ | 501 だが、Go SDK が 5xx を回数上限なしでリトライするため、呼び出し側の期限まで戻らない |
| 404 | ルートが未実装。SDK は「見つからない」と報告し、リソースがない場合と区別できない |
| — | その SDK では該当なし、または未確認 |

### バケット

| 操作 | Go | Ruby | 備考 |
| --- | --- | --- | --- |
| 既定値で作成 | ✅ | ✅ | Ruby が送る既定の `autoclass:{enabled:false}` も受け付ける |
| versioning・labels・CORS 付きで作成 | ✅ | ✅ | Go は作成時に versioning、labels と CORS は更新で確認 |
| リージョン（`ASIA-NORTHEAST1`）で作成 | 501⏳ | 501 | 対応は `US`・`EU`・`ASIA` のみ |
| 事前定義 ACL・既定オブジェクト ACL 付きで作成 | 501⏳ | 501 | |
| UBLA 付きで作成 | 501⏳ | — | Ruby は更新でも 501 |
| 階層型名前空間付きで作成 | 501⏳ | — | |
| website・requester pays・autoclass・オブジェクト保持付きで作成 | 501⏳（オブジェクト保持） | 501 | |
| 重複作成で 409 | ✅ | ✅ | |
| 取得・存在しないバケット・`exists?` | ✅ | ✅ | |
| 一覧（プロジェクト、prefix） | ✅ | ✅ | |
| labels・versioning・CORS・PAP・storage class・既定イベントホールドの更新 | ✅ | ✅ | |
| ライフサイクルの更新 | 501 | 501 | |
| 保持ポリシーの更新 | 501 | 501 | |
| 保持ポリシーのロック | 501⏳ | 501 | Go は前段の保持ポリシー作成で失敗 |
| UBLA の更新 | 501 | 501 | |
| website・logging・requester pays の更新 | 501 | 501 | |
| soft delete ポリシー・autoclass・既定 KMS 鍵の更新 | 501 | 501 | |
| RPO の更新 | — | 501 | |
| 古い metageneration の前提条件で 412 | ✅ | ✅ | |
| IAM の取得・設定・testIamPermissions | ✅ | ✅ | |
| バケット ACL の一覧・追加・削除 | 404 | 404 | Ruby の `acl.public!` は 501 |
| 既定オブジェクト ACL の一覧・追加 | 404 | 404 | |
| 通知の作成・一覧・削除 | 404 | 404 | |
| 空のバケットの削除、空でなければ 409 | ✅ | ✅ | |
| soft delete したバケットの復元 | — | 404 | |

### プロジェクトのリソース

| 操作 | Go | Ruby |
| --- | --- | --- |
| サービスアカウントのメールアドレス | 404 | 404 |
| HMAC 鍵の作成・一覧 | 404 | 404 |

### オブジェクト

| 操作 | Go | Ruby | 備考 |
| --- | --- | --- | --- |
| 単純アップロード | ✅ | ✅ | Ruby の `StringIO` で `content_type` 省略も動作 |
| 再開可能アップロード（256 KiB × 3 チャンク） | ✅ | ✅ | |
| コンテンツヘッダーとカスタムメタデータ | ✅ | ✅ | |
| チェックサム不一致の拒否 | ✅ | ✅ | Go は CRC32C、Ruby は MD5 |
| 既存オブジェクトへの `ifGenerationMatch=0` で 412 | ✅ | ✅ | |
| 事前定義 ACL 付きアップロード | 501 | 501 | |
| KMS 鍵付きアップロード | 501 | 501 | |
| 顧客指定の暗号鍵（書き込み・読み取り・ローテーション） | 501 | 501 | |
| ダウンロード（Go は XML 既定と JSON、Ruby は `StringIO` とパス） | ✅ | ✅ | |
| 範囲ダウンロード | ✅ | ✅ | |
| 存在しないオブジェクトは not found | ✅ | ✅ | |
| gzip のトランスコードと圧縮のままの読み取り | ✅ | ✅ | |
| メタデータの取得 | ✅ | ✅ | |
| メタデータ・content type の更新 | ✅ | ✅ | |
| 一時ホールドとイベントホールド | ✅ | ✅ | |
| customTime | ✅ | ✅ | |
| rewrite による storage class の変更 | — | ✅ | |
| オブジェクト保持 | 501⏳ | 501 | Go は保持を有効にしたバケットの作成で失敗 |
| 古い metageneration の前提条件で 412 | ✅ | ✅ | |
| コピー（別バケットへの上書き指定付きを含む） | ✅ | ✅ | |
| rewrite | ✅ | ✅ | Go はスイートで確認 |
| compose | ✅ | ✅ | |
| move | 404 | 404 | |
| オブジェクト ACL の一覧・追加 | 404 | 404 | Ruby の `acl.public!` は 501 |
| 削除、存在しなければ not found | ✅ | ✅ | |
| 旧世代の読み取り・バージョン一覧・世代指定の削除 | ✅ | ✅ | |
| soft delete したオブジェクトの復元 | 404 | 404 | |
| soft delete 済みオブジェクトの一覧 | 501⏳ | 501 | |

### 一覧

| オプション | Go | Ruby | 備考 |
| --- | --- | --- | --- |
| prefix・delimiter・ページング | ✅ | ✅ | |
| matchGlob | ✅ | ✅ | |
| start・end offset | ✅ | — | Ruby 1.62.1 には引数がない |
| includeTrailingDelimiter・fields 指定 | ✅ | — | Ruby は未確認 |
| includeFoldersAsPrefixes | 501⏳ | 501 | |

### 署名・認証・通信

| 領域 | Go | Ruby | 備考 |
| --- | --- | --- | --- |
| V4 署名付き URL の GET・PUT とレスポンス上書き（strict） | ✅ | ✅ | スイート。Go v1.70.0 は canonical host から既定外のポートを落とす。Ruby は `bucket_bound_hostname` が必要 |
| POST ポリシー V4 のフォームアップロード | 501 | 501 | Ruby の V2 `post_object` も 501 |
| strict 認証（viewer・creator・admin） | ✅ | ✅ | スイート |
| 明示的な CA での HTTPS | ✅ | — | Go スイート。Ruby の HTTPS は `just check-sdk` では実行していない |
| gRPC クライアント | 使えない | — | gRPC のリスナーがなく、Go の gRPC クライアントは `Unavailable` で失敗 |
| `public_url` | — | `storage.googleapis.com` を指す | SDK の動作 |

## 気づいた点

- Go SDK は既定で 408・429・すべての 5xx（501 を含む）を回数上限なしでリトライする。そのため 501 になる
  冪等な呼び出し（バケット作成、保持ポリシーのロック、一覧のオプション）は呼び出し側の期限まで戻らない。
  Ruby は既定のリトライで約 9 秒後に失敗する。
- 未実装の JSON ルート（ACL、通知、HMAC 鍵、サービスアカウント、move、restore）は 404 `Not Found` を返す。
  SDK からは未対応の操作ではなく、バケットやオブジェクトがないように見える。
- 受け付けて無視した設定は見つからなかった。

## 実施していないこと

実クラウドとの比較、Ruby の HTTPS での再実行、Rails Active Storage、ほかの SDK、ベンチマーク、リソース測定は
行っていない。プローブのスクリプトは一度きりのもので、コミットしていない。
