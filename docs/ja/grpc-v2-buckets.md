# Storage v2 gRPCのバケット操作

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-STORAGE-001〜004、BEHAVIOR-RPC-001とAUDIT-008のバケット部分です。正本は[英語の契約](../en/slices/grpc-v2-buckets.md)、状態の正本は`compatibility/features.json`です。

`grpc_listen`を設定したサーバーへ`STORAGE_EMULATOR_HOST_GRPC`を向けると、通常のGo `storage.NewGRPCClient(ctx)`でバケットの作成・取得・一覧・削除を利用できます。JSONと同じカタログを使うため、相互に作成したバケットが見えます。新しい設定はありません。

CreateBucketの保持・soft delete・PAP・CORS・Autoclassは共有の型付き検証を再利用します。
作成全体の共有型は未整備のため、残りの作成項目にはJSON insertパーサーを使い、PARTIALを維持します。UpdateBucketは共有更新型を直接作ります。

名前は`projects/_/buckets/NAME`です。作成時のparentは空または`projects/_`、所有projectは`bucket.project`に`projects/<id>`で指定します。全Storage v2 RPCの共通入口でrouting metadataを検査し、ストリーミングは最初のメッセージ、rewrite継続はtokenの名前と照合します。`bucket`・`project`・rewriteの`source_bucket`とRapidの`appendable`を検査し、GoのDirectPath診断を受け付けます。未対応の意味を持つキーはUNIMPLEMENTEDです。バケット名はbucket・object両方のRPCで設定済みの互換プロファイルに従います。不正な名前や食い違う値へのstatusは、SPEC-GRPC-ROUTING-NAMESに記録したローカルな選択です。GoとRustのgRPC SDKでの確認は残っています。

作成時はlocation、storage class、labels、CORS、versioning、default event hold、retention、soft delete、IAM設定の共有JSON検証を使います。Autoclassは[共有の設定規則（英語）](../en/slices/bucket-autoclass-storage-class.md)に従い、出力専用の時刻は拒否します。object retentionは`enable_object_retention`で有効にします（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。read maskでは`etag`・`autoclass`・`lifecycle`・`object_retention`も選べます。未対応の意味を持つ設定・read maskは`UNIMPLEMENTED`です。取得・削除のメタ世代条件、prefixとページング、非空バケットの削除拒否に対応します。strictでは共有認可を使い、保護されたバケットへの未認証アクセスは`UNAUTHENTICATED`、登録済みprincipalの権限不足は`PERMISSION_DENIED`になります。`allUsers`で許可されたGet/DeleteはJSONと同様に匿名でも成功し、公開アクセス防止も共有の判定に従います。

課金先を指定する`x-goog-user-project`は、2026-11-13から全Storage v2 RPCで無視します（空値も含みます）。JSONの`userProject`も同じく無視します（SPEC-SDK-UNSUPPORTED）。gcs-mimicのバケットではRequester Paysを有効にできないため、課金先が結果を変えることはありません。

projectはIDでも、バケットが返す番号でも指定でき、ListBucketsのページトークンはどちらの書き方でも続きます（BEHAVIOR-COMMON-014）。Storage v1とオブジェクトの転送方式間検証は対象外です。残りの作業は下の台帳のケースのとおりです。AUDIT-008はPARTIALを維持します。性能測定・コンテナ・実クラウド検証は対象外です。

2026-10-28に、型検査、gRPCユニット8件、Goの担当・foundationテスト、最終の`RUST_TEST_THREADS=8 just check`と`just check-sdk`が成功しました。HTTPプロファイルのため、既存のGo V2署名HTTPS専用ケースはSKIPです。台帳の6行は確認済みの範囲と未対応を分けてPARTIALとし、英日対応表を再生成しました。

バケットの作成・更新で拒否する設定（`billing`・`custom_placement_config`・`encryption`・`hierarchical_namespace`・`ip_filter`・`logging`・`rpo`・`website`）は保存されないため、`read_mask`で指定でき、未設定として返します。Cloud Storageも、これらを持たないバケットには未設定として答えました（ケース`grpc-bucket-unset-settings-mask`、2026-11-15に記録）。JavaのSDKは既定のバケット読み取りマスクでこれらをすべて指定します。`satisfies_pzs`は意図して拒否し（SPEC-GRPC-UNMODELED-SETTINGS）、入れ子のパスは未対応のためUNIMPLEMENTEDのままです。

バケットのリソースは、2026-11-17に記録したCloud StorageのUpdateBucketの応答全体（ケース`bkt-gaps-legacy-class-region`のステップ`grpc-update-bucket-class-regional`）と、Javaの既定マスクでのGetBucketの応答全体（ケース`grpc-bucket-unset-settings-mask`）に、無視するフィールドなしで一致します。バケットのACLと既定のオブジェクトACLでは、バケットのプロジェクトのチームを指すエントリに、プロジェクトIDで書いた`entity_alt`（`project-owners-{id}`）を付けます。オブジェクトのACLと同じです。ほかのプロジェクト番号のチームには付けません。その番号がどのIDを指すかをgcs-mimicは知らないからです。`create_time`と`update_time`は、記録どおりミリ秒までです。保持と論理削除の有効日時、Autoclassの時刻、均一アクセスのロック時刻も、JSONと同じくミリ秒に切り捨てます（Storage v2では未記録の推定です）。保持期間が0の論理削除ポリシーには、JSONと同じく`effective_time`を付けません。

2026-11-14に、バケットのオプションを`grpc_bucket_options_reject_only_what_is_unsupported`で固定しました。CreateBucketの`predefined_acl`・`predefined_default_object_acl`（バケットは作られない）と、GetBucketの入れ子の`read_mask`は、オプション名を含むErrorInfo付きUNIMPLEMENTEDを返します。実機記録（UTC で 2026-11-13、ケース`grpc-unsupported-options`）で、Cloud StorageはListBucketsの`return_partial_success`を受け付け、`*`と名前を混ぜた`read_mask`をINVALID_ARGUMENTで断ると分かったので、gcs-mimicも同じ応答に揃えました。入れ子の`read_mask`もCloud Storageは受け付けますが、gcs-mimicでは記録済みのローカルな拒否として残します。RPC-STORAGE-002・003・004には成功ケース`grpc_unsupported_option_rejections`として記録し、ほかの未完了ケースは変えていません。

## 台帳のケース

2026-11-18 に、台帳の各行を、確かめられる振る舞いごとの受け入れケースに分けました。実機の Cloud Storage では実行しないため、Google が定めておらず記録もない応答はローカルな選択とし、固定するテストと合わせて scoped difference に記録します。概要は次のとおりです。

- 意図した拒否（SPEC-GRPC-UNMODELED-SETTINGS の scoped difference）: JSON API も保存しない作成時の設定（`billing`・`encryption`・`logging`・`website`・`custom_placement_config`・`hierarchical_namespace`・`ip_filter`・`satisfies_pzs=true`・`DEFAULT`以外の`rpo`）、GetBucket の`satisfies_pzs`の read mask、RPC がルーティングしない`x-goog-request-params`のキー（リダイレクトのルーティングトークンを含む）。
- 記録済みのローカルな選択: 小数を含む soft delete の期間の拒否（SPEC-GRPC-LOCAL-CHOICES）と、不正または食い違うルーティング値への INVALID_ARGUMENT（SPEC-GRPC-ROUTING-NAMES）。
- 実装が必要なもの: 入れ子のバケット read mask（Cloud Storage は受け付けます）、CreateBucket の lifecycle・ACL・predefined ACL、出力専用フィールドを拒否せず無視すること、小数を含む保持期間を proto のとおり秒単位に切り捨てること。
- テストが必要なもの: 論理削除済みのオブジェクト・非現行の世代・未完了の multipart upload・未確定の appendable オブジェクトだけを持つバケットや、soft delete ポリシーを持つバケットの DeleteBucket、DeleteBucket のエラーの順序、ListBuckets での`satisfies_pzs`の拒否、ルーティングを確かめる Go と Rust の gRPC SDK。
- 判断が必要なもの: バケットの metageneration 条件の検証規則（GetBucket と DeleteBucket は検証せずに比較し、UpdateBucket と LockBucketRetentionPolicy は 0・負の値・両条件の同時指定を拒否します）、サーバーで`grpc-timeout`を適用するか、gzip のメッセージ圧縮に対応するか。
