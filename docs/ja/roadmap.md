# ロードマップ

[English](../en/roadmap.md)

2026-11-26 に更新しました。以下は着手の順番で、日付は約束しません。

## 目標

gcs-mimic は、開発とテストのためのローカルな Cloud Storage エミュレーターです。
公式の Cloud Storage SDK と、それを使う Rails Active Storage や`gcloud storage`などのツールが、接続先の設定を変えるだけで動くことを目指します。

次のことは目標にしません。

- Cloud Storage と応答をバイト単位で一致させること
- Cloud Storage のすべての API と管理サービスに対応すること
- fake-gcs-server の完全な上位互換になること

公式の資料が動作を決めていない箇所では、SDK が動き続ける動作を選び、ローカルの動作として記録します。
互換性は、公式 SDK、Google のドキュメント、googleapis の proto で判断します。リポジトリの中のものは Cloud Storage に接続しません（[理由（英語）](../en/development.md#no-live-cloud-storage-contact)）。

## 対象範囲

この節は、[機能台帳](../../compatibility/features.json)の`NOT_APPLICABLE`の行が参照する製品プロファイルです。
状態の原本は台帳で、[機能表](feature-matrix.md)で一覧できます。

対象にするもの:

- 公式 SDK が JSON API で呼ぶ操作。バケットとオブジェクトの作成・取得・更新・削除、media・multipart・再開可能アップロード、ダウンロードと範囲指定、一覧、copy・rewrite・compose・move、メタデータと前提条件、世代、hold、retention、soft delete です。
- SDK の gRPC クライアントが使う範囲の Storage v2 gRPC と、すでに提供している旧 Storage v1 の RPC。
- SDK とブラウザーが使う XML API。署名付き URL、署名付き POST policy、XML マルチパートアップロード、Rails Active Storage のダイレクトアップロードです。
- SDK の利用で関わる権限制御。IAM ポリシー、ACL、均一なバケットレベルのアクセス、公開アクセスの防止、HMAC キー、strict でのローカル ID です。
- エミュレーターの運用。設定、バケットの初期設定、seed と移行、fake-gcs-server からの移行、ネイティブ版とコンテナイメージです。

対象にしないもの:

- Storage Control v2、フォルダー、管理フォルダー、階層型名前空間
- Storage Insights、Storage Intelligence、Storage Batch Operations
- Anywhere Cache、Rapid Cache、Cloud Storage の MCP サーバー
- 長時間実行オペレーション、ロケーション、FeatureConfigs
- XML API のバケット設定・ACL・retention・暗号化・HMAC のサブリソース、XML の複数オブジェクト削除、XML の compose、HTML フォームのアップロード
- オブジェクトコンテキスト、バケットの移転、一括復元、オブジェクトの IAM、ウェブサイトのルーティング、利用ログ、IP フィルター、暗号化の強制、RPO、Autoclass の自動変更
- Google の基盤とほかのサービス。KMS、OAuth トークンの発行、VPC Service Controls、CDN、課金、Monitoring と Logging、Storage Transfer Service、Firebase、Cloud Storage FUSE
- ベンチマークの比較表、fake-gcs-server との性能比較、ローカル開発の規模を超える資源の上限

すでに`PARTIAL`の行は、コード・状態・未達ケースをそのまま残します。
対象外の機能も、公式 SDK の利用で必要になれば、新しいスライスとして戻します。

## 次の作業

1. **SDK が動き続けるようにする。** [9つの SDK のスコアカード](../../sdk-scorecard/README.md)と SDK のテストを、毎回の公開で通します。利用者から届いた SDK の使い方を追加し、テストが外部へ通信しないことを確かめます（DESIGN-088）。
2. **SDK の利用で当たる不足を埋める。** `PARTIAL`の行の未達ケースは、SDK の利用で必要になったときに対応します。行を完了させること自体は目的にしません。
3. **いつでも公開できる状態を保つ。** 毎回の公開で、配布と依存の条件を満たします。

## 1.0.0

1.0.0 は、scorecard の 9 つの SDK が固定したバージョンで、リリースするコミットの採点対象ケースにすべて通り、`1.x` の間は文書化した使い方とデータディレクトリを保って更新できることを意味します。
`PARTIAL`の行がすべて完了したという意味ではありません。
7 日間のリリース候補の試用期間を含む条件の全体は、[バージョン方針](release-policy.md#100の条件)にあります。
