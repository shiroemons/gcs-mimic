# 初回の準備と基盤作成

共通プロンプトと、このプロンプトを新規リポジトリで使用します。`project/` の内容がリポジトリ直下にある前提です。

```text
添付の計画一式を使い、gcs-mimicのP0基盤だけを実装してください。
共通実装プロンプトとAGENTS.mdに従ってください。指示と報告は日本語、実装・コミット・正本ドキュメントは英語です。

1. プロジェクト名・CLI名・リポジトリ名をgcs-mimicに統一する。設定の環境変数prefixはGCS_MIMIC_とする。
2. docs/en/dependency-policy.mdに従い、公式情報で確認した最新stable Rustと最新安定版ライブラリを採用する。対応する最新editionを確認し、具体的なtoolchain/MSRV、必要最小限の依存、Cargo.lock、モジュール構成を用意する。日付付きの計画の版をそのまま使わない。
3. CLI、設定ファイル、環境変数、設定優先順位、HTTPサーバー、health/readiness、正常終了、リソース上限、注入可能な時計の基盤を用意する。
4. JSON/XML/gRPCと共通処理層・永続化層の境界を定義する。本文保存とメタデータcommitの整合性方針を記録する。
5. 非rootのDocker multi-stage build、linux/amd64とlinux/arm64、Compose、必要な書込先・永続ボリューム・初期データ入力先を用意する。
6. compatibility/features.jsonの検証と英語表生成をRust xtaskで実装する。ID、依存、状態、再現精度、IMPLEMENTEDに必要な証拠を検証し、表の生成差分をCIで確認する。
7. フォーマット、lint、基盤のテスト、Dockerの起動・停止、資料の整合性をCIで確認する。固定版の再現可能なCIと最新stable確認のCI、Rust・crate・Docker・実際に使うSDK等の毎日の更新検出を用意する。メジャー更新や採用不能な最新版も明示的に追跡する。
8. 英語のREADMEと導入手順を作り、ソース取得→準備→build→起動→health→停止まで実際に通す。
9. getting-started-spec.mdの後半にあるCRUD、SDK、公開/署名URL、永続化の手順は、未実装の段階と実装予定を明示する。架空の成功例にしない。

今回GCSの業務APIは実装しないでください。未対応ルートは成功のプレースホルダーを返さないでください。
実行済みの検証で証明できたP0の基盤項目だけを実装済みにし、GCS機能は未実装のまま維持してください。

プロジェクト名: gcs-mimic
```
