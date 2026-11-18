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
appendableのhandleは、追記や確定後も発行時のサイズまでしか読めない。
有効期限と削除後のstatusは、Googleが定めておらず実機でも確かめないため、記録済みのローカルな選択とする（SPEC-GRPC-READ-HANDLES。2026-11-18からRPC-STORAGE-016とBEHAVIOR-RPC-004のscoped difference）。
strict では所有プロジェクトも固定し、別プロジェクトで同名バケットを作り直した場合の再接続を拒否する。
ルーティングトークンは、エミュレーターがストリームをリダイレクトしないため、意図してUNIMPLEMENTEDで拒否する（SPEC-GRPC-UNMODELED-SETTINGS）。
最初の応答のオブジェクトは、storage.proto の 1185〜1187 行が定めるとおり、非推奨の`read_mask`に従う。なし・空の mask は`acl`と`owner`以外の全フィールドを返す。`*`は、フィールドでも、proto が代替として挙げる`x-goog-fieldmask`ヘッダーでも、ACL と owner を含む全フィールドを返し、ReadObject の`*`と同じオブジェクトになる。
それ以外の`read_mask`（なし・空・`*`以外）と、`*`以外の`x-goog-fieldmask`ヘッダーは ErrorInfo 付きの UNIMPLEMENTED とする。
非推奨のこのフィールドの説明は ReadObjectRequest からの写しで、BidiReadObjectResponse にない`checksummed_data`が常に含まれると述べ、gRPC メタデータによる代替の文が途中で切れている（storage.proto の 1183〜1190 行）。
このため mask がどの応答パスを選ぶかが定まらない。
read handle と世代・metageneration の前提条件の併用も ErrorInfo 付きの UNIMPLEMENTED とする。
条件を handle が開き直す世代に適用するのかライブオブジェクトに適用するのかを proto が定めていないためで、条件を使うときは handle なしで開き直す。
どちらの拒否も記録済みのローカルな選択とする（SPEC-GRPC-LOCAL-CHOICES）。
最初のメッセージがない、または`read_object_spec`がないストリームと、後のメッセージで`read_object_spec`を繰り返すストリームは INVALID_ARGUMENT。
後から追記された内容の読み取りは BEHAVIOR-RAPID-005 で扱う。
顧客指定の暗号鍵は [gRPC の CSEK](grpc-v2-customer-supplied-keys.md) のとおり、開くたびと再開のたびに spec の鍵を検査する。
エラー詳細には、受理した未完了範囲のread_idをすべて含める。失敗した範囲は固有のstatusを保ち、ほかの範囲は再試行を促すABORTEDにする。
通常のバッチ内で一部の範囲が不正な場合も、バッチの全read_idを報告する。100件を超えるバッチは受理しない。
protoは未完了の各read_idの報告を求めるがstatusを定めていないため、ABORTEDは記録済みのローカルな選択とする（SPEC-GRPC-PENDING-RANGES）。
protoはエントリの順序も定めていないため、失敗した範囲、キューにある範囲（キューの順）、拒否したメッセージの残り（リクエストの順）という順序も、同じく記録済みのローカルな選択とする。入力の上限はSPEC-GRPC-LOCAL-CHOICESにローカルな上限として記録する。
詳細内のgoogle.rpc.Statusのcodeとmessageは、I/O障害やread_id重複でも外側のgRPC statusと一致させる。
read handleの照合は長さを確認した後、32バイトすべてを比較する。
新しい設定・依存・カタログ変更・ポーリングは加えていない。
エラーはストレージのドメインエラーから`src/grpc/v2/status.rs`で直接 Status にし、
ストレージの失敗はUNAVAILABLE、未対応の設定はErrorInfo付きUNIMPLEMENTEDにする。
ほかの Storage v2 の呼び出しと同じく`x-goog-request-params`が必要で（なければ INVALID_ARGUMENT）、
最初のメッセージが届いた時点で、ルーティングされたバケットを`read_object_spec`のバケットと比べる。
Cloud Storage は記録に使ったプロジェクトでは BidiReadObject を拒否したため、この要求も比較もこの RPC では記録しておらず、
記録できた RPC（ケース`grpc-probe-routing`と`grpc-probe-inferred-routing`）からの推測である。

Go SDK v1.70.0 は再接続時に未完了範囲を 1 メッセージにまとめる。
100 件を超えた場合は proto の上限に従って再試行されない INVALID_ARGUMENT を返す。
この再接続条件は SDK 側の制限として残る。中断のない 600 件の Add/Wait は既定の呼び方で検証済み。

2026-11-18に、テストのなかった上記の応答をテストで固定した。既存の挙動を固定するテストで、最初の実行から通過した。
`the_spec_refuses_redirects_nondefault_masks_and_conditions_on_a_handle`は spec を検査し、ルーティングトークン、既定以外の read mask、read handle と 4 種類の条件それぞれの併用が ErrorInfo 付きの UNIMPLEMENTED になること、空のルーティングトークンとなし・空・`*`の mask は受け付けること、負の世代が INVALID_ARGUMENT になること、負の世代条件と metageneration 条件は拒否せず、オブジェクトを読むときに比べることを確かめる。比べる扱いは GetObject と ReadObject で記録した応答（2026-11-18、ケース`grpc-probe-inferred-conditions`）からの推定です。
`the_stream_refuses_a_missing_or_late_spec_and_a_field_mask_header`はストリームを wire 経由で実行し、メッセージなし、spec のない最初のメッセージ、後からの spec が INVALID_ARGUMENT になること、`*`以外の`x-goog-fieldmask`ヘッダーが応答の前に ErrorInfo を 1 件持つ UNIMPLEMENTED になり、`*`なら読めることを確かめる。
`bidi_range_errors_list_the_failed_range_first_then_queue_and_request_order`は`read_range_errors`のエントリの順序を固定する。
`cargo test --locked --offline --lib grpc::`の 270 件が通過した。SDK のテストは実行していない。

同じ 2026-11-18 のその後に、`*`の mask でオブジェクトの ACL と owner を返すようにした。
それまでの BidiReadObject は`*`を受け付けても既定のフィールドで応答していたため、proto の「全フィールド」や ReadObject の`*`が含む ACL と owner が最初の応答になかった。
`a_wildcard_mask_returns_the_acl_and_owner_the_default_omits`は変更より先に書き、ACL が空で owner がないため失敗した。
変更後は、`*`の read mask、`*`の`x-goog-fieldmask`ヘッダー、その両方のどれでも ReadObject の`*`と同じオブジェクトを返し、なし・空の mask は ReadObject の既定と同じく ACL と owner を含まない。
ReadObject と BidiReadObject はこのオブジェクトを同じヘルパーで組み立てる。`the_spec_refuses_redirects_nondefault_masks_and_conditions_on_a_handle`は、`validate_spec`が`*`を全フィールド、なし・空を既定と読むことを確かめる。
`cargo test --locked --offline --lib grpc::`の 273 件が通過した。SDK のテストは実行していない。

2026-11-04にRustの対象10テストとACLのwire 2テストが通過した。
削除・上書き・バケット再作成・RAM予算解放・TTL・FIFO・appendableのサイズ上限を確認した。
Goの上書き後の再開期待値はNOT_FOUNDに更新したが、更新後のSDKスイートは未実行。

以下は2026-10-28時点の検証記録。
Go の高水準 API、wire、strict の対象テストと Rust の範囲境界テストは通過。
最終ソースでの `just check-sdk` も Go 全体と Ruby の各シナリオまで通過。
`RUST_TEST_THREADS=8 just check` も整形、native/Windows/Linux clippy、Rust 全テスト、台帳・生成表まで通過。
mainの`31fb5b0`へ載せ直した最終ソースでも、対象テストと全体検証を各1回実行し、すべて通過。
全体検証の詳細は英語版に記録する。コンテナ・ガイド・ベンチマーク・実クラウド検証は対象外。
