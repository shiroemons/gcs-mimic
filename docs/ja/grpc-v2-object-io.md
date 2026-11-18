# Storage v2 gRPC のオブジェクト入出力

<!-- markdownlint-configure-file {"MD013": false} -->

対象は RPC-STORAGE-011、RPC-STORAGE-013、RPC-STORAGE-014、RPC-STORAGE-015、RPC-STORAGE-018、RPC-STORAGE-019、RPC-STORAGE-020、RPC-STORAGE-022、RPC-STORAGE-023、BEHAVIOR-RPC-003 です。広い API 全体の状態は PARTIAL とし、正本の台帳から両言語の対応表を生成します。

確定済みupload_idの完了応答は、ストリームの先頭で再開した場合に返します。
途中で別ストリームが確定した場合、後続データにはFAILED_PRECONDITIONを返します。
それ以外の並行ストリームの挙動は未検証です。

`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444` のように、設定済みの h2c リスナーを指定すると、Go の通常の `storage.NewGRPCClient(ctx)` で Writer、NewReader、NewRangeReader、Attrs、Delete、Objects を使えます。Writer のチャンクサイズを変更せず、小さい本文と 5 MiB の本文を検証しました。JSON と gRPC は同じオブジェクト、世代、権限、ホールド、保持、soft delete を使います。

WriteObject と BidiWriteObject は同じ処理を使い、チェックサムを確認してから共有カタログで確定します。StartResumableWrite の upload_id で再開でき、QueryWriteStatus は永続化済みのオフセットまたは確定したメタデータを返します。オフセット不一致、CRC32C・MD5 不一致、世代条件の失敗で既存の本文を置き換えません。ReadObject は範囲と負のオフセット、CRC32C、世代指定に対応し、一覧は prefix・delimiter・ページングに対応します。

確定済みのupload_idで書き込みを再試行すると、ライブオブジェクトが変わっていても、元の完了時のメタデータをOKで返します。未確定のupload_idストリームを正常にhalf-closeすると、flushやstate_lookupがなくても永続化済みサイズを返します。非再開可能な単発書き込みは、protoの指定どおり、最後のメッセージにfinish_writeが必要です。名前の検証はJSONと共通で、`.`・`..`・ACME challenge接頭辞を拒否し、タブは許可します。ストレージのI/Oエラーは共有の分類で返します。

ListObjectsは共有の型付きクエリを直接作り、JSONのURLや一覧リソースを組み立てません。発行済みのNoAclページトークンは引き続き使えます。組み合わせのエラーにはprotoのフィールド名を使い、不正なglobには具体的な構文の説明を返します。

CancelResumableWrite は、JSON の session URI への DELETE と同じカタログ処理で resumable session を取り消します。書き込み途中のバイトも破棄され、オブジェクトは公開されません。取り消した後の QueryWriteStatus・WriteObject・BidiWriteObject、2 回目の取消、未知・不正な upload_id は NOT_FOUND になります。確定済みの upload は FAILED_PRECONDITION です。別の書き込みが session を使用中なら `catalog_wait_timeout_ms` まで待ち、それでも解放されなければ再試行できる UNAVAILABLE です。strict では `storage.objects.create` が必要で、viewer は PERMISSION_DENIED、匿名は UNAUTHENTICATED になり、session はそのまま残ります。確定済み・2 回目・書き込み中の status は Google が定めていないため、ローカルで選んだ挙動です。

本文の送受信は上限付きで、本文全体を集約しません。既存の 4 MiB の受信メッセージ制限、64 KiB の本文チャンク、max_object_bytes を使い、新しい設定は追加しません。ListObjects のページは約 2 MiB で区切って次ページのトークンを返し、進むように必ず 1 件を含みます。単発書き込みは resumable session の保存枠を使いません。

未対応は、単発書き込みの途中の flush/state_lookup、ネストした read_mask、soft-deletedオブジェクトのGetObjectなどです。記録済みのStorage v2のownerは`entity`だけを持ち、gcs-mimicの応答も同じです。soft-deletedの一覧は[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)で扱います。顧客指定の暗号鍵は[gRPC の CSEK](grpc-v2-customer-supplied-keys.md)のとおりに扱います。GetObjectと一覧のmaskは`Object`の全トップレベルフィールドと`*`を受け付けます。応答にはJSONと同じ`etag`と`update_storage_class_time`が入ります。`acl`と`owner`はmaskで指定したとき（または`*`）だけ返し、JSONの`projection=full`と同じ共有処理で作るため、UBLAが有効な場合や`storage.objects.getIamPolicy`がない場合は空になります。appendable オブジェクトは [Rapid append の契約](grpc-v2-rapid-append.md) で扱います。受け付けて無視せず UNIMPLEMENTED を返します。メタデータに 8 KiB の上限はありません。Cloud Storage は WriteObject で 8192 バイトと 40,000 バイトの値を、UpdateObject で 20,000 バイトの値を保存しました（2026-11-15 に記録したケース grpc-custom-metadata-limit）。JSON にエンコードした固定メタデータとカスタムメタデータが、JSON・XML と共通のローカル上限 40 KiB を超えると、公開せずに INVALID_ARGUMENT を返します。明示的な resumable session は共有処理に従います。件数は既定で無制限で（`max_resumable_sessions`で上限を設定できます）、StartResumableWrite から 7 日で失効します。エラーの順序や、同じ upload_id の複数ストリームの競合は、下の台帳のケースのとおり未確定です。

upload_id を使うストリームでは、非最終メッセージの object_checksums は UNIMPLEMENTED で拒否します。StartResumableWrite または finish_write=true で指定してください。最初のメッセージでの指定は上流の契約では有効です。以前は、共有セッションが変更した checksum を永続化できないことを理由にしていました。しかしセッションのチェックポイントは、各メッセージのオプションを、統合した checksum も含めてオフセットと同じ更新で保存します（`src/storage/catalog/resumable.rs`の`checkpoint_resumable_output`）。このため、最初のメッセージでの受け付けは未対応の作業（BEHAVIOR-RPC-003 の`stream_resumable_first_checksums`）として残します。拒否してもオフセットは進みません。StartResumableWrite の checksum は切断・再起動後も検証され、不一致ではオブジェクトを作成せずセッションを無効化します。

2026-10-28 に cargo check、clippy、Rust の対象 5 テスト、Go の対象 4 テストが通りました。Go では strict、再開、WriteObject、チェックサム、JSON 相互運用、ホールド・保持・soft delete も確認しています。最終ソースで `RUST_TEST_THREADS=8 just check` と `just check-sdk` が通り、すべてのライブラリテスト、その他の Rust テスト、Go/Ruby 全体、台帳・対応表の検証を終えました。詳細は[英語の契約](../en/slices/grpc-v2-object-io.md)を参照してください。性能測定、コンテナ、ガイド通し、実クラウドの検証は対象外です。

## 台帳のケース

2026-11-18 に、台帳の各行を、確かめられる振る舞いごとの受け入れケースに分けました。実機の Cloud Storage では実行しないため、Google が定めておらず記録もない応答はローカルな選択とし、固定するテストと合わせて scoped difference に記録します。概要は次のとおりです。

- 意図した拒否（SPEC-GRPC-UNMODELED-SETTINGS の scoped difference）: 書き込みリソースの`kms_key`とオブジェクトのコンテキスト、ListObjects のコンテキスト用`filter`。
- 記録済みのローカルな選択（SPEC-GRPC-LOCAL-CHOICES）: CancelResumableWrite が返す FAILED_PRECONDITION・NOT_FOUND・UNAVAILABLE と、`storage.objects.create`を持つ誰でも取り消せること。RPC-STORAGE-013 には未完了のケースがなく、所有者のレビュー待ちです。
- 実装が必要なもの: 書き込みリソースの`predefined_acl`・`acl`・`retention`と出力専用フィールド（proto が出力専用とする`resource.checksums`を検証し続けるかも含む）、GetObject の`soft_deleted`、ネストした read_mask、オブジェクトの read_mask を取得前に検証すること（現在は返すオブジェクトごとに検証するため、空のページでは不正な mask でも成功します）、再開可能・単発の書き込みと QueryWriteStatus の`persisted_data_checksums`（現在は appendable だけが返します）、単発ストリームの途中の flush と state_lookup、upload_id ストリームの最初のメッセージの`object_checksums`、`finish_write`の後にストリームの終了を待つ間も writer lease を持ち続ける WriteObject の再開。
- テストが必要なもの: GetObject の`restore_token`の拒否、DeleteObject・ReadObject・WriteObject のエラーの順序、Storage v2 での 7 日の失効、QueryWriteStatus が返す値が減らないこと、セッションのチェックポイントでの書き込み・同期の失敗とクラッシュ。
- 判断が必要なもの: オブジェクトの前提条件の検証規則（GetObject・ReadObject・BidiReadObject・DeleteObject・WriteObjectSpec は緩く、変更系の RPC は metageneration 条件の 0 と match・not-match の同時指定を拒否します）、再開したストリームの最初のメッセージが永続化済みオフセットより前から始めてよいか（proto の`write_offset`の説明（1411〜1415 行）と WriteObject の説明（480〜489 行）が食い違っています）、使用中のセッションに対して待つか ABORTED を返すか、SDK の half-close を確認したうえで BidiWriteObject の`finish_write`後のメッセージを報告するか、ReadObject が gzip のオブジェクトを展開するか。
