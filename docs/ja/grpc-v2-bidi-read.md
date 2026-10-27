# Storage v2 双方向範囲読み取り

対象は RPC-STORAGE-016、BEHAVIOR-RPC-004。両行とも PARTIAL。
契約の正本は [英語版](../en/slices/grpc-v2-bidi-read.md)。

`grpc_listen` を有効にし、Go の `STORAGE_EMULATOR_HOST_GRPC` をそのアドレスに指定する。
`storage.NewGRPCClient(ctx, storage.WithGRPCBidiReads())` で
`NewMultiRangeDownloader`、複数回の `Add`、`Wait`、`Close` が使える。
前提データは JSON API で用意する。

最初のメッセージで対象の世代と前提条件を確認し、metadata と read_handle を返す。
後続メッセージでも範囲を追加でき、各範囲に read_id と range_end を付け、
64 KiB 以下のチャンクごとに CRC32C を返す。長さ 0 は末尾まで、負の開始位置は末尾基準。
末尾を超える開始位置と負の長さは OUT_OF_RANGE。
strict の読み取り権限は初回・再接続の両方で共有の認可処理を使って確認する。

範囲の上限は 1 メッセージあたり 100 件。キューが 100 件以上の間は次のメッセージを読まず、
完了に合わせて入力を再開する。直前のメッセージを含むキューは最大 199 件。
I/O は 1 本で範囲を交互に処理し、応答キューは 2 件。大量の Add を総数だけで拒否しない。
read handle はプロセス内に最大 64 個を保持し、オブジェクト上書き後も選んだ世代を読む。
古い handle の追い出しと再起動後は FAILED_PRECONDITION になり、handle を省いて開き直す。
strict では所有プロジェクトも固定し、別プロジェクトで同名バケットを作り直した場合の再接続を拒否する。
handle と前提条件の併用、暗号化、ルーティングトークン、既定以外の read mask は UNIMPLEMENTED。
範囲エラーの詳細は不正な範囲の read_id を含むが、ほかの未完了範囲をすべて列挙する処理は未対応。
新しい設定・依存・カタログ変更・ポーリングは加えていない。
エラーは共有の`status::from_json`を使い、JSONの503はUNAVAILABLE、
未対応の501はErrorInfo付きUNIMPLEMENTEDへ変換する。

Go SDK v1.70.0 は再接続時に未完了範囲を 1 メッセージにまとめる。
100 件を超えた場合は proto の上限に従って再試行されない INVALID_ARGUMENT を返す。
この再接続条件は SDK 側の制限として残る。中断のない 600 件の Add/Wait は既定の呼び方で検証済み。

Go の高水準 API、wire、strict の対象テストと Rust の範囲境界テストは通過。
最終ソースでの `just check-sdk` も Go 全体と Ruby の各シナリオまで通過。
`RUST_TEST_THREADS=8 just check` も整形、native/Windows/Linux clippy、Rust 全テスト、台帳・生成表まで通過。
mainの`31fb5b0`へ載せ直した最終ソースでも、対象テストと全体検証を各1回実行し、すべて通過。
全体検証の詳細は英語版に記録する。コンテナ・ガイド・ベンチマーク・実クラウド検証は対象外。
