# Storage v2 gRPCのバケット操作

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-STORAGE-001〜004、BEHAVIOR-RPC-001とAUDIT-008のバケット部分です。正本は[英語の契約](../en/slices/grpc-v2-buckets.md)、状態の正本は`compatibility/features.json`です。

`grpc_listen`を設定したサーバーへ`STORAGE_EMULATOR_HOST_GRPC`を向けると、通常のGo `storage.NewGRPCClient(ctx)`でバケットの作成・取得・一覧・削除を利用できます。JSONと同じカタログを使うため、相互に作成したバケットが見えます。新しい設定はありません。

名前は`projects/_/buckets/NAME`です。作成時のparentは空または`projects/_`、所有projectは`bucket.project`に`projects/<id>`で指定します。Go SDKはこの形式を自動で組み立てます。routing metadataの`bucket`・`project`を復号して要求と照合します。Go SDKが付けるDirectPath診断は受け付けます。

作成時はlocation、storage class、labels、CORS、versioning、default event hold、retention、soft delete、IAM設定の共有JSON検証を使います。未対応の意味を持つ設定・read maskは`UNIMPLEMENTED`です。取得・削除のメタ世代条件、prefixとページング、非空バケットの削除拒否に対応します。strictでは共有認可を使い、保護されたバケットへの未認証アクセスは`UNAUTHENTICATED`、登録済みprincipalの権限不足は`PERMISSION_DENIED`になります。`allUsers`で許可されたGet/DeleteはJSONと同様に匿名でも成功し、公開アクセス防止も共有の判定に従います。

課金先を指定する`x-goog-user-project`は、空値を明示した場合もErrorInfo付き`UNIMPLEMENTED`で拒否します。JSONの明示的な`userProject`も未対応です。Go SDKの既定呼び出しと`Bucket.UserProject("")`はこのmetadataを送らないため利用できます。

Storage v1、オブジェクトの転送方式間検証、project IDから番号への解決、バケット自体のsoft delete・restore、未完了のXML multipart・Rapidとの組み合わせ、未対応フィールドの完全な互換性は残っています。AUDIT-008はPARTIALを維持します。性能測定・コンテナ・実クラウド検証は対象外です。

2026-10-28に、型検査、gRPCユニット8件、Goの担当・foundationテスト、最終の`RUST_TEST_THREADS=8 just check`と`just check-sdk`が成功しました。HTTPプロファイルのため、既存のGo V2署名HTTPS専用ケースはSKIPです。台帳の6行は確認済みの範囲と未対応を分けてPARTIALとし、英日対応表を再生成しました。
