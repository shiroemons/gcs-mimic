# 旧Storage v1 gRPC

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-LEGACY-001〜012で、いずれもPARTIALです。fake-gcs-server v1.56.1から移行するための旧プロトコルであり、GoogleのGA APIやEXACT互換性を表しません。台帳のfidelityはLOCAL_SIMULATIONです。正本は[英語の契約](../en/slices/grpc-v1-legacy.md)です。

## 対象RPC

比較対象はfake-gcs-serverのコミット896ece51e49eed0946089f3089f6343855c0d106にあるinternal/grpc/server.goです。そこで実装されている12 RPCだけを対応範囲とします。

| 機能ID | RPC | 対応範囲 |
| --- | --- | --- |
| RPC-LEGACY-001 | GetBucket | バケット名、メタ世代条件、共有メタデータ |
| RPC-LEGACY-002 | DeleteBucket | 原子的なメタ世代条件、空でないバケットの削除拒否 |
| RPC-LEGACY-003 | InsertBucket | 明示したプロジェクト、名前、location、class、labels、versioning、default event hold |
| RPC-LEGACY-004 | UpdateBucket | default event hold、versioning、classの更新とlabels全体の置換。省略したラベルは削除し、メタ世代条件を適用 |
| RPC-LEGACY-005 | ListBuckets | 明示したプロジェクト、prefix、最大1000件のページと継続トークン |
| RPC-LEGACY-006 | InsertObject | 複数メッセージの非再開アップロード、offset、CRC32C、16進MD5、原子的な公開条件 |
| RPC-LEGACY-007 | GetObject | メタデータ、世代選択、世代・メタ世代の4条件 |
| RPC-LEGACY-008 | DeleteObject | 世代選択、4条件、共有のhold・retention・versioning・soft delete |
| RPC-LEGACY-009 | UpdateObject | 固定・カスタムメタデータの置換と指定したhold、空文字による固定値のクリア |
| RPC-LEGACY-010 | PatchObject | 空でない固定値とカスタムメタデータのマージ、任意のevent hold |
| RPC-LEGACY-011 | ComposeObject | 同一バケットの1〜32ソース、ソース世代・条件、宛先条件、上限付き合成 |
| RPC-LEGACY-012 | ListObjects | prefix、delimiter、trailing delimiter、versions、最大1000件のページと継続トークン |

既存のgrpc_listenを設定したh2cポートにv1とv2を載せます。例は`--grpc-listen 127.0.0.1:4444`です。新しい設定や依存はありません。認可とカタログ・公開処理はJSONと共用し、strictの権限を迂回しません。JSONで作ったバケット・オブジェクトをv1で参照でき、v1の書き込みや更新はJSONで参照できます。

本文のメッセージ上限は4 MiB、メタデータ上限は8 KiBです。共有の書き込みには64 KiBを2枠で渡し、総量には既存のmax_object_bytesを適用します。finish_writeとEOFを確認するまで公開しません。誤ったoffsetやチェックサム、finish不足は公開せず失敗します。前提条件と権限は共有処理が公開と同じトランザクションで再確認します。通信中にSQLiteのトランザクションを保持しません。

## 制限と固定方法

棚卸し外のRPCは、入力やEOFを待たずにErrorInfo付きUNIMPLEMENTEDを返します。GetObjectMediaも対象外なので、本文の読み出しはJSONを使います。billing、quota、common fields、暗号化、ACL/full projection、KMS、非対応メタデータ、resumable upload ID、reference data、空でないPatchObjectのupdate_maskも同じエラーです。空のcommon設定と省略値は受け付けます。

ListBucketsはプロジェクト指定が必要です。比較対象のプロジェクトなし全件一覧は提供しません。UpdateBucketのlabelsは固定protoに従い全体を置き換えます。比較対象はdefault event holdしか更新しないため、ラベルの契約はprotoを根拠にしています。PatchObjectは空でない値のマージであり、固定値を消す場合はUpdateObjectを使います。完全なACL・lifecycle・暗号化の応答、数値プロジェクト番号、メタデータの大きい一覧、クラッシュ・競合の全組み合わせは未検証です。

取得したBucket/Objectの値を変更してUpdateへ書き戻せます。指定された出力専用値が現在値と一致するときは受け付け、異なる値はUNIMPLEMENTEDで拒否します。Objectは共有更新トランザクションの中で照合し、Bucketは認可したスナップショットのメタ世代を更新条件に使って競合を拒否します。

v1のサービス定義はGoogle側で削除されているため、既存のGo生クライアントに合うgoogleapisコミットb2e2bc62fab90e6829e62d3d189906d9b79899e4にv1だけを固定しています。共通importは段9の固定を維持し、wrappers.protoにはprotobuf v36.2を使います。SHA-256と再生成手順は英語の契約とsrc/grpc/proto/SHA256SUMSにあります。通常ビルドにはprotocが不要です。v2アダプターと既存v2生成物は変更しません。

## 検証

受け入れケースは実装前に記録しました。型検査、clippy、24 protoのSHA-256、対象Goテスト3件が通っています。Goの生クライアントで12 RPC、JSONとの相互参照、合成した本文、継続ページ、棚卸し外の全メソッドの2秒以内の拒否、入力を送らないストリームの拒否を確認しました。offset・CRC32C・MD5・finish不足で公開されないこと、条件失敗、strictの閲覧と更新拒否も確認しました。

最終ソースの`just check-sdk`も成功しました。temporary_hold=falseでの解除、新しいv1テスト、既存Go全体、Rubyの自己検査・移行・permissive・strictを含みます。HTTP環境では既存のGo V2署名テストだけがHTTPSを要するためスキップされ、v1テストのスキップはありません。台帳603行と英日対応表の整合確認、変更した3文書のMarkdown lintも成功しています。

ラベル全体の置換、GetBucket/GetObjectの戻り値を変更して書き戻す操作、異なる出力専用値の拒否もGoで確認しています。`cargo test --locked --offline --lib grpc::v1::`の同値判定テストも成功しました。

途中の`just check`では既存のTLS時間依存テストが1回失敗しました。失敗箇所はtests/tls.rs:999の初回HTTP 200確認です。TLSのコードや期限を変更せず、単独再実行と次の全体実行では成功しました。

main 414114cに載せ直した最終ソースで、`RUST_TEST_THREADS=8 just check`と`just check-sdk`がともに成功しました。native・Windows・Linuxのclippy、532件のライブラリテスト、残りの単体・統合・docテスト、台帳と生成表、オフラインのスクリプト検査を含みます。Rustの失敗・無視はなく、TLSも9件すべて成功しました。

非対応フィールドと未検証のクラッシュ・競合ケースを残しているため、12行ともPARTIALです。ベンチマーク、コンテナビルド、ガイドの通し実行、HTTPSでの再実行、実クラウドへの接続は行いません。
