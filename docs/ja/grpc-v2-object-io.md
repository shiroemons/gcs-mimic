# Storage v2 gRPC のオブジェクト入出力

<!-- markdownlint-configure-file {"MD013": false} -->

対象は RPC-STORAGE-011、RPC-STORAGE-013、RPC-STORAGE-014、RPC-STORAGE-015、RPC-STORAGE-018、RPC-STORAGE-019、RPC-STORAGE-020、RPC-STORAGE-022、RPC-STORAGE-023、BEHAVIOR-RPC-003 です。広い API 全体の状態は PARTIAL とし、正本の台帳から両言語の対応表を生成します。

`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444` のように、設定済みの h2c リスナーを指定すると、Go の通常の `storage.NewGRPCClient(ctx)` で Writer、NewReader、NewRangeReader、Attrs、Delete、Objects を使えます。Writer のチャンクサイズを変更せず、小さい本文と 5 MiB の本文を検証しました。JSON と gRPC は同じオブジェクト、世代、権限、ホールド、保持、soft delete を使います。

WriteObject と BidiWriteObject は同じ処理を使い、チェックサムを確認してから共有カタログで確定します。StartResumableWrite の upload_id で再開でき、QueryWriteStatus は永続化済みのオフセットまたは確定したメタデータを返します。オフセット不一致、CRC32C・MD5 不一致、世代条件の失敗で既存の本文を置き換えません。ReadObject は範囲と負のオフセット、CRC32C、世代指定に対応し、一覧は prefix・delimiter・ページングに対応します。

CancelResumableWrite は、JSON の session URI への DELETE と同じカタログ処理で resumable session を取り消します。書き込み途中のバイトも破棄され、オブジェクトは公開されません。取り消した後の QueryWriteStatus・WriteObject・BidiWriteObject、2 回目の取消、未知・不正な upload_id は NOT_FOUND になります。確定済みの upload は FAILED_PRECONDITION、別の書き込みが session を使用中なら再試行できる UNAVAILABLE です。strict では `storage.objects.create` が必要で、viewer は PERMISSION_DENIED、匿名は UNAUTHENTICATED になり、session はそのまま残ります。確定済み・2 回目・書き込み中の status は Google が定めていないため、ローカルで選んだ挙動です。

本文の送受信は上限付きで、本文全体を集約しません。既存の 4 MiB メッセージ制限、64 KiB の本文チャンク、max_object_bytes を使い、新しい設定は追加しません。単発書き込みは resumable session の保存枠を使いません。

未対応は、単発書き込みの途中の flush/state_lookup、ネストした read_mask、gRPC の soft-deleted 取得・一覧、暗号化・ACL、Rapid append などです。受け付けて無視せず UNIMPLEMENTED を返します。メタデータはローカル上限 8 KiB、明示的な resumable session は共有処理の保存上限 128 件・7 日間に従います。プロバイダーと完全に一致するエラー順序や、同じ upload_id の複数ストリームの競合は未検証です。

upload_id を使うストリームでは、非最終メッセージの object_checksums は UNIMPLEMENTED で拒否します。StartResumableWrite または finish_write=true で指定してください。最初のメッセージでの指定は上流の契約では有効ですが、共有セッションの既存インターフェースでは変更した checksum を永続化できず、切断後に検証を抜けるためです。拒否してもオフセットは進みません。StartResumableWrite の checksum は切断・再起動後も検証され、不一致ではオブジェクトを作成せずセッションを無効化します。

2026-10-28 に cargo check、clippy、Rust の対象 5 テスト、Go の対象 4 テストが通りました。Go では strict、再開、WriteObject、チェックサム、JSON 相互運用、ホールド・保持・soft delete も確認しています。最終ソースで `RUST_TEST_THREADS=8 just check` と `just check-sdk` が通り、すべてのライブラリテスト、その他の Rust テスト、Go/Ruby 全体、台帳・対応表の検証を終えました。詳細は[英語の契約](../en/slices/grpc-v2-object-io.md)を参照してください。性能測定、コンテナ、ガイド通し、実クラウドの検証は対象外です。
