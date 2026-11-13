# Storage v2 gRPCのバケット操作

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-STORAGE-001〜004、BEHAVIOR-RPC-001とAUDIT-008のバケット部分です。正本は[英語の契約](../en/slices/grpc-v2-buckets.md)、状態の正本は`compatibility/features.json`です。

`grpc_listen`を設定したサーバーへ`STORAGE_EMULATOR_HOST_GRPC`を向けると、通常のGo `storage.NewGRPCClient(ctx)`でバケットの作成・取得・一覧・削除を利用できます。JSONと同じカタログを使うため、相互に作成したバケットが見えます。新しい設定はありません。

CreateBucketの保持・soft delete・PAP・CORS・Autoclassは共有の型付き検証を再利用します。
作成全体の共有型は未整備のため、残りの作成項目にはJSON insertパーサーを使い、PARTIALを維持します。UpdateBucketは共有更新型を直接作ります。

名前は`projects/_/buckets/NAME`です。作成時のparentは空または`projects/_`、所有projectは`bucket.project`に`projects/<id>`で指定します。全Storage v2 RPCの共通入口でrouting metadataを検査し、ストリーミングは最初のメッセージ、rewrite継続はtokenの名前と照合します。`bucket`・`project`・rewriteの`source_bucket`とRapidの`appendable`を検査し、GoのDirectPath診断を受け付けます。未対応の意味を持つキーはUNIMPLEMENTEDです。バケット名はbucket・object両方のRPCで設定済みの互換プロファイルに従います。Googleの不正名ステータスと全SDKのrouting組み合わせはSPEC-GRPC-ROUTING-NAMESで未確認です。

作成時はlocation、storage class、labels、CORS、versioning、default event hold、retention、soft delete、IAM設定の共有JSON検証を使います。Autoclassは[共有の設定規則（英語）](../en/slices/bucket-autoclass-storage-class.md)に従い、出力専用の時刻は拒否します。object retentionは`enable_object_retention`で有効にします（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。read maskでは`etag`・`autoclass`・`lifecycle`・`object_retention`も選べます。未対応の意味を持つ設定・read maskは`UNIMPLEMENTED`です。取得・削除のメタ世代条件、prefixとページング、非空バケットの削除拒否に対応します。strictでは共有認可を使い、保護されたバケットへの未認証アクセスは`UNAUTHENTICATED`、登録済みprincipalの権限不足は`PERMISSION_DENIED`になります。`allUsers`で許可されたGet/DeleteはJSONと同様に匿名でも成功し、公開アクセス防止も共有の判定に従います。

課金先を指定する`x-goog-user-project`は、全Storage v2 RPCで空値を明示した場合もErrorInfo付き`UNIMPLEMENTED`で拒否します。ストリームの入力前に検査します。JSONの明示的な`userProject`も未対応です。Go SDKの既定呼び出しと`Bucket.UserProject("")`はこのmetadataを送らないため利用できます。

Storage v1、オブジェクトの転送方式間検証、project IDから番号への解決、バケット自体のsoft delete・restore、未完了のXML multipart・Rapidとの組み合わせ、未対応フィールドの完全な互換性は残っています。AUDIT-008はPARTIALを維持します。性能測定・コンテナ・実クラウド検証は対象外です。

2026-10-28に、型検査、gRPCユニット8件、Goの担当・foundationテスト、最終の`RUST_TEST_THREADS=8 just check`と`just check-sdk`が成功しました。HTTPプロファイルのため、既存のGo V2署名HTTPS専用ケースはSKIPです。台帳の6行は確認済みの範囲と未対応を分けてPARTIALとし、英日対応表を再生成しました。

2026-11-14に、テストのなかった拒否を`grpc_bucket_rpcs_reject_unsupported_options_without_side_effects`で固定しました。CreateBucketの`predefined_acl`・`predefined_default_object_acl`（バケットは作られない）、GetBucketの入れ子・ワイルドカード混在の`read_mask`、ListBucketsの`return_partial_success`は、いずれもオプション名を含むErrorInfo付きUNIMPLEMENTEDを返します。`cargo test --locked --offline --lib grpc::`の168件が成功しました。RPC-STORAGE-002・003・004には成功ケース`grpc_unsupported_option_rejections`として記録し、ほかの未完了ケースは変えていません。
