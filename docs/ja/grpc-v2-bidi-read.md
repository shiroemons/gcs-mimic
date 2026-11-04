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
read handleは識別子と発行時のサイズ上限だけを持ち、ReaderやBlobの予算を保持しない。
再接続時は同じ世代を認可付きで開き直す。削除・非versioningの上書き後はNOT_FOUNDになり、handleも破棄する。
versioningで残る世代は読め、進行中のストリームは上書き後も元のReaderを使う。
最大1024件、注入時計で10分の有効期限とし、追い出し・失効・再起動後はFAILED_PRECONDITIONになる。
appendableのhandleは、追記や確定後も発行時のサイズまでしか読めない。有効期限と削除後のstatusはGoogleで未確認のローカルな規則。
strict では所有プロジェクトも固定し、別プロジェクトで同名バケットを作り直した場合の再接続を拒否する。
handle と前提条件の併用、暗号化、ルーティングトークン、既定以外の read mask は UNIMPLEMENTED。
エラー詳細には、受理した未完了範囲のread_idをすべて含める。失敗した範囲は固有のstatusを保ち、ほかの範囲は再試行を促すABORTEDにする。
通常のバッチ内で一部の範囲が不正な場合も、バッチの全read_idを報告する。100件を超えるバッチは受理しない。
ABORTEDの選択はGoogleで未観測のため、SPEC-GRPC-PENDING-RANGESに記録している。
詳細内のgoogle.rpc.Statusのcodeとmessageは、I/O障害やread_id重複でも外側のgRPC statusと一致させる。
read handleの照合は長さを確認した後、32バイトすべてを比較する。
新しい設定・依存・カタログ変更・ポーリングは加えていない。
エラーは共有の`status::from_json`を使い、JSONの503はUNAVAILABLE、
未対応の501はErrorInfo付きUNIMPLEMENTEDへ変換する。

Go SDK v1.70.0 は再接続時に未完了範囲を 1 メッセージにまとめる。
100 件を超えた場合は proto の上限に従って再試行されない INVALID_ARGUMENT を返す。
この再接続条件は SDK 側の制限として残る。中断のない 600 件の Add/Wait は既定の呼び方で検証済み。

2026-11-04にRustの対象10テストとACLのwire 2テストが通過した。
削除・上書き・バケット再作成・RAM予算解放・TTL・FIFO・appendableのサイズ上限を確認した。
Goの上書き後の再開期待値はNOT_FOUNDに更新したが、更新後のSDKスイートは未実行。

以下は2026-10-28時点の検証記録。
Go の高水準 API、wire、strict の対象テストと Rust の範囲境界テストは通過。
最終ソースでの `just check-sdk` も Go 全体と Ruby の各シナリオまで通過。
`RUST_TEST_THREADS=8 just check` も整形、native/Windows/Linux clippy、Rust 全テスト、台帳・生成表まで通過。
mainの`31fb5b0`へ載せ直した最終ソースでも、対象テストと全体検証を各1回実行し、すべて通過。
全体検証の詳細は英語版に記録する。コンテナ・ガイド・ベンチマーク・実クラウド検証は対象外。
