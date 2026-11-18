# Storage v2 gRPC のオブジェクト入出力

<!-- markdownlint-configure-file {"MD013": false} -->

対象は RPC-STORAGE-011、RPC-STORAGE-013、RPC-STORAGE-014、RPC-STORAGE-015、RPC-STORAGE-018、RPC-STORAGE-019、RPC-STORAGE-020、RPC-STORAGE-022、RPC-STORAGE-023、BEHAVIOR-RPC-003 です。広い API 全体の状態は PARTIAL とし、正本の台帳から両言語の対応表を生成します。

確定済みupload_idの完了応答は、ストリームの先頭で再開した場合に返します。
途中で別ストリームが確定した場合、後続データにはFAILED_PRECONDITIONを返します。
それ以外の並行ストリームの挙動は下の「書き込みストリーム」で説明します。

`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444` のように、設定済みの h2c リスナーを指定すると、Go の通常の `storage.NewGRPCClient(ctx)` で Writer、NewReader、NewRangeReader、Attrs、Delete、Objects を使えます。Writer のチャンクサイズを変更せず、小さい本文と 5 MiB の本文を検証しました。JSON と gRPC は同じオブジェクト、世代、権限、ホールド、保持、soft delete を使います。

WriteObject と BidiWriteObject は同じ処理を使い、チェックサムを確認してから共有カタログで確定します。StartResumableWrite の upload_id で再開でき、QueryWriteStatus は永続化済みのオフセットまたは確定したメタデータを返します。オフセット不一致、CRC32C・MD5 不一致、世代条件の失敗で既存の本文を置き換えません。ReadObject は範囲と負のオフセット、CRC32C、世代指定に対応し、一覧は prefix・delimiter・ページングに対応します。

確定済みのupload_idで書き込みを再試行すると、ライブオブジェクトが変わっていても、元の完了時のメタデータをOKで返します。未確定のupload_idストリームを正常にhalf-closeすると、flushやstate_lookupがなくても永続化済みサイズを返します。非再開可能な単発書き込みは、protoの指定どおり、最後のメッセージにfinish_writeが必要です。名前の検証はJSONと共通で、`.`・`..`・ACME challenge接頭辞を拒否し、タブは許可します。ストレージのI/Oエラーは共有の分類で返します。

ListObjectsは共有の型付きクエリを直接作り、JSONのURLや一覧リソースを組み立てません。発行済みのNoAclページトークンは引き続き使えます。組み合わせのエラーにはprotoのフィールド名を使い、不正なglobには具体的な構文の説明を返します。

CancelResumableWrite は、JSON の session URI への DELETE と同じカタログ処理で resumable session を取り消します。書き込み途中のバイトも破棄され、オブジェクトは公開されません。取り消した後の QueryWriteStatus・WriteObject・BidiWriteObject、2 回目の取消、未知・不正な upload_id は NOT_FOUND になります。確定済みの upload は FAILED_PRECONDITION です。別の書き込みが session を使用中なら `catalog_wait_timeout_ms` まで待ち、それでも解放されなければ再試行できる UNAVAILABLE です。strict では `storage.objects.create` が必要で、viewer は PERMISSION_DENIED、匿名は UNAUTHENTICATED になり、session はそのまま残ります。確定済み・2 回目・書き込み中の status は Google が定めていないため、ローカルで選んだ挙動です。

本文の送受信は上限付きで、本文全体を集約しません。既存の 4 MiB の受信メッセージ制限、64 KiB の本文チャンク、max_object_bytes を使い、新しい設定は追加しません。ListObjects のページは約 2 MiB で区切って次ページのトークンを返し、進むように必ず 1 件を含みます。単発書き込みは resumable session の保存枠を使いません。

未対応は、ネストした read_mask、soft-deletedオブジェクトのGetObject、所有者の`entity_id`、期限切れセッションに対するQueryWriteStatusの検証などです。soft-deletedの一覧は[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)で扱います。顧客指定の暗号鍵は[gRPC の CSEK](grpc-v2-customer-supplied-keys.md)のとおりに扱います。GetObjectと一覧のmaskは`Object`の全トップレベルフィールドと`*`を受け付けます。応答にはJSONと同じ`etag`と`update_storage_class_time`が入ります。`acl`と`owner`はmaskで指定したとき（または`*`）だけ返し、JSONの`projection=full`と同じ共有処理で作るため、UBLAが有効な場合や`storage.objects.getIamPolicy`がない場合は空になります。appendable オブジェクトは [Rapid append の契約](grpc-v2-rapid-append.md) で扱います。受け付けて無視せず UNIMPLEMENTED を返します。メタデータに 8 KiB の上限はありません。Cloud Storage は WriteObject で 8192 バイトと 40,000 バイトの値を、UpdateObject で 20,000 バイトの値を保存しました（2026-11-15 に記録したケース grpc-custom-metadata-limit）。JSON にエンコードした固定メタデータとカスタムメタデータが、JSON・XML と共通のローカル上限 40 KiB を超えると、公開せずに INVALID_ARGUMENT を返します。明示的な resumable session は共有処理の保存上限 128 件・7 日間に従います。

## 書き込みストリーム

WriteObject と BidiWriteObject は、単発・再開・appendable のどの書き込みでもメッセージごとに検査し、拒否するメッセージには次の status を返します。`src/grpc/v2/object_io/write/message_tests.rs` のテストが各ケースを RPC サービス経由で送り、status、メッセージ、それまでに返した応答、書き込み後に残る状態を固定しています。固定した proto が status を定めているのは object_size の不一致（OUT_OF_RANGE）だけで、ほかの status、文言、優先順位はエミュレーターの選択です（ギャップ SPEC-GRPC-WRITE-ERRORS）。拒否した単発書き込みは何も公開せず、拒否した再開メッセージは何も書き込まずセッションを永続化済みオフセットのまま残し、拒否した appendable の最初のメッセージはオブジェクトの作成も引き継ぎもしません。

| メッセージ | status |
| --- | --- |
| 最初のメッセージがない、または 2 通目以降に最初のメッセージがある | INVALID_ARGUMENT |
| 負の `write_offset`、受信済みバイトを超えるオフセット、再開ストリームの最初のオフセットが永続化済みサイズと異なる | OUT_OF_RANGE |
| チャンクの CRC32C、オブジェクト全体のチェックサム、MD5 の形式、`object_checksums` の食い違い | INVALID_ARGUMENT |
| 単発書き込みの 2 通目以降、または appendable で、`finish_write` のないメッセージの `object_checksums` | INVALID_ARGUMENT |
| upload_id の `finish_write` のないメッセージの `object_checksums`、単発書き込みの `finish_write` のないメッセージの `flush`/`state_lookup` | UNIMPLEMENTED |
| `object_size` の不一致 | OUT_OF_RANGE |
| `max_object_bytes` の超過 | RESOURCE_EXHAUSTED |
| 鍵なしで始めた書き込みの後続メッセージの鍵 | INVALID_ARGUMENT（顧客指定鍵の文言） |
| 単発書き込みの最後のメッセージに `finish_write` がない | INVALID_ARGUMENT |
| WriteObject で `finish_write` の後に届いたメッセージ | INVALID_ARGUMENT |

1 通のメッセージが複数の規則に反する場合、単発書き込みは後続メッセージの鍵、最初のメッセージ、`flush`/`state_lookup`、チェックサムの位置、MD5 の長さ、チェックサムの食い違い、オフセットをチャンクの CRC32C より先に検査します。再開書き込みは最初のメッセージとチェックサムの位置をセッションの参照より先に検査し、その後に鍵、公開条件、チェックサム、オフセットをチャンクの CRC32C より先に検査します。appendable 書き込みは鍵、最初のメッセージ、チャンクの CRC32C をチェックサムの位置、MD5、オフセットより先に検査し、最初のメッセージはオブジェクトの作成や引き継ぎより先に検査します。オブジェクト全体の不一致の文言は、単発書き込みではアダプター、再開と appendable では共有カタログのものです。

- 所有者判断で維持する挙動: BidiWriteObject は `finish_write` に resource を返して OK で終わり、それ以降のメッセージは読みません。proto はその後のメッセージが「エラーになる」としています。WriteObject はストリームの終わりまで読み、終わりのメッセージ自身の検査の後で INVALID_ARGUMENT を返します（ギャップ SPEC-GRPC-WRITE-STREAMS）。
- WriteObject は再開アップロードのセッションをリースする前に `finish_write` の先を読みます。ストリームを閉じないクライアントがセッションを占有しないので、別のストリームや CancelResumableWrite が使えます。
- 所有者判断で維持する挙動: 再開ストリームの最初の `write_offset` は、proto のとおり永続化済みサイズと一致する必要があります。永続化済みバイトを再送するだけの値を含め、それ以外は OUT_OF_RANGE です（status は proto が定めていません）。2 通目以降のメッセージは永続化済みバイトを再送してよく、その部分は読み飛ばします。
- 同じ upload_id の複数ストリームについて proto は定めていません。別ストリームが書き込み中のセッションへのメッセージは待たずに UNAVAILABLE で失敗し、何も変えません。交互に書くストリームはそれぞれ永続化済みオフセットから続け、別ストリームが永続化したバイトに重なる再送は比較せずに読み飛ばすため、先に永続化したバイトが残ります。別ストリームが確定したアップロードには FAILED_PRECONDITION を返します。QueryWriteStatus は書き込み中のメッセージを待たず、最後のチェックポイントで答えます。
- WriteObjectResponse、BidiWriteObjectResponse、QueryWriteStatusResponse は、永続化済みサイズ（0 を含む）を返すとき、proto のとおり永続化済みバイトの CRC32C と MD5 を `persisted_data_checksums` に入れます。resource の応答には入れません。0.26.0 より前のリリースが作ったセッションには途中のダイジェストがないため、確定するか期限が切れるまで入りません。appendable の応答は従来どおり CRC32C だけです。
- WriteObject、BidiWriteObject、StartResumableWrite は、predefined ACL とエミュレーターが扱わない resource フィールドを、何も作る前に ErrorInfo 付きの UNIMPLEMENTED で拒否します。StartResumableWrite は appendable も拒否します（ギャップ SPEC-GRPC-WRITE-REFUSALS）。

upload_id を使うストリームでは、非最終メッセージの object_checksums は UNIMPLEMENTED で拒否します。StartResumableWrite または finish_write=true で指定してください。最初のメッセージでの指定は上流の契約では有効ですが、共有セッションの既存インターフェースでは変更した checksum を永続化できず、切断後に検証を抜けるためです。拒否してもオフセットは進みません。StartResumableWrite の checksum は切断・再起動後も検証され、不一致ではオブジェクトを作成せずセッションを無効化します。

2026-11-18 に `cargo test --locked --offline --lib grpc::v2` が、上記の書き込みストリームのテストを含めて通りました。`grpc_io_write_object_awaits_the_stream_end_without_holding_the_session` と `append_refuses_an_invalid_opening_message_before_creating_or_taking_over` は修正前に失敗しました。修正前は WriteObject がストリームの終わりを待つ間セッションのリースを保持し、CRC32C の誤り・負のオフセット・MD5 を含む appendable の最初のメッセージが、拒否される前にオブジェクトを作成するか現在の書き手を置き換えていました。`grpc_resumable_persisted_sizes_carry_the_persisted_checksums` は `persisted_data_checksums` を入れる前に失敗しました。Go SDK はこれらの変更の後に再実行していません。

2026-10-28 に cargo check、clippy、Rust の対象 5 テスト、Go の対象 4 テストが通りました。Go では strict、再開、WriteObject、チェックサム、JSON 相互運用、ホールド・保持・soft delete も確認しています。最終ソースで `RUST_TEST_THREADS=8 just check` と `just check-sdk` が通り、すべてのライブラリテスト、その他の Rust テスト、Go/Ruby 全体、台帳・対応表の検証を終えました。詳細は[英語の契約](../en/slices/grpc-v2-object-io.md)を参照してください。性能測定、コンテナ、ガイド通し、実クラウドの検証は対象外です。
