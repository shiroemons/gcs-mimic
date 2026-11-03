# Storage v2 gRPC のオブジェクト入出力

<!-- markdownlint-configure-file {"MD013": false} -->

対象は RPC-STORAGE-011、RPC-STORAGE-013、RPC-STORAGE-014、RPC-STORAGE-015、RPC-STORAGE-018、RPC-STORAGE-019、RPC-STORAGE-020、RPC-STORAGE-022、RPC-STORAGE-023、BEHAVIOR-RPC-003 です。広い API 全体の状態は PARTIAL とし、正本の台帳から両言語の対応表を生成します。

`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444` のように、設定済みの h2c リスナーを指定すると、Go の通常の `storage.NewGRPCClient(ctx)` で Writer、NewReader、NewRangeReader、Attrs、Delete、Objects を使えます。Writer のチャンクサイズを変更せず、小さい本文と 5 MiB の本文を検証しました。JSON と gRPC は同じオブジェクト、世代、権限、ホールド、保持、soft delete を使います。

WriteObject と BidiWriteObject は同じ処理を使い、チェックサムを確認してから共有カタログで確定します。StartResumableWrite の upload_id で再開でき、QueryWriteStatus は永続化済みのオフセットまたは確定したメタデータを返します。オフセット不一致、CRC32C・MD5 不一致、世代条件の失敗で既存の本文を置き換えません。ReadObject は範囲と負のオフセット、CRC32C、世代指定に対応し、一覧は prefix・delimiter・ページングに対応します。

確定済みのupload_idで書き込みを再試行すると、ライブオブジェクトが変わっていても、元の完了時のメタデータをOKで返します。未確定のupload_idストリームを正常にhalf-closeすると、flushやstate_lookupがなくても永続化済みサイズを返します。非再開可能な単発書き込みは、protoの指定どおり、最後のメッセージにfinish_writeが必要です。名前の検証はJSONと共通で、`.`・`..`・ACME challenge接頭辞を拒否し、タブは許可します。ストレージのI/Oエラーは共有の分類で返します。

ListObjectsは共有の型付きクエリを直接作り、JSONのURLや一覧リソースを組み立てません。発行済みのNoAclページトークンは引き続き使えます。組み合わせのエラーにはprotoのフィールド名を使い、不正なglobには具体的な構文の説明を返します。

CancelResumableWrite は、JSON の session URI への DELETE と同じカタログ処理で resumable session を取り消します。書き込み途中のバイトも破棄され、オブジェクトは公開されません。取り消した後の QueryWriteStatus・WriteObject・BidiWriteObject、2 回目の取消、未知・不正な upload_id は NOT_FOUND になります。確定済みの upload は FAILED_PRECONDITION、別の書き込みが session を使用中なら再試行できる UNAVAILABLE です。strict では `storage.objects.create` が必要で、viewer は PERMISSION_DENIED、匿名は UNAUTHENTICATED になり、session はそのまま残ります。確定済み・2 回目・書き込み中の status は Google が定めていないため、ローカルで選んだ挙動です。

本文の送受信は上限付きで、本文全体を集約しません。既存の 4 MiB メッセージ制限、64 KiB の本文チャンク、max_object_bytes を使い、新しい設定は追加しません。単発書き込みは resumable session の保存枠を使いません。

未対応は、単発書き込みの途中の flush/state_lookup、ネストした read_mask、soft-deletedオブジェクトのGetObject、暗号化、所有者の`entity_id`とACLの`entity_alt`などです。soft-deletedの一覧は[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)で扱います。GetObjectと一覧のmaskは`Object`の全トップレベルフィールドと`*`を受け付けます。応答にはJSONと同じ`etag`と`update_storage_class_time`が入ります。`acl`と`owner`はmaskで指定したとき（または`*`）だけ返し、JSONの`projection=full`と同じ共有処理で作るため、UBLAが有効な場合や`storage.objects.getIamPolicy`がない場合は空になります。appendable オブジェクトは [Rapid append の契約](grpc-v2-rapid-append.md) で扱います。受け付けて無視せず UNIMPLEMENTED を返します。メタデータはローカル上限 8 KiB、明示的な resumable session は共有処理の保存上限 128 件・7 日間に従います。プロバイダーと完全に一致するエラー順序や、同じ upload_id の複数ストリームの競合は未検証です。

upload_id を使うストリームでは、非最終メッセージの object_checksums は UNIMPLEMENTED で拒否します。StartResumableWrite または finish_write=true で指定してください。最初のメッセージでの指定は上流の契約では有効ですが、共有セッションの既存インターフェースでは変更した checksum を永続化できず、切断後に検証を抜けるためです。拒否してもオフセットは進みません。StartResumableWrite の checksum は切断・再起動後も検証され、不一致ではオブジェクトを作成せずセッションを無効化します。

2026-10-28 に cargo check、clippy、Rust の対象 5 テスト、Go の対象 4 テストが通りました。Go では strict、再開、WriteObject、チェックサム、JSON 相互運用、ホールド・保持・soft delete も確認しています。最終ソースで `RUST_TEST_THREADS=8 just check` と `just check-sdk` が通り、すべてのライブラリテスト、その他の Rust テスト、Go/Ruby 全体、台帳・対応表の検証を終えました。詳細は[英語の契約](../en/slices/grpc-v2-object-io.md)を参照してください。性能測定、コンテナ、ガイド通し、実クラウドの検証は対象外です。
