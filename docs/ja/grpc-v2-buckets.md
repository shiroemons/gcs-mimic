# Storage v2 gRPCのバケット操作

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-STORAGE-001〜004、BEHAVIOR-RPC-001とAUDIT-008のバケット部分です。下の「2026-11-18のバケットの変更」は、UpdateBucket・LockBucketRetentionPolicy・SetIamPolicy（RPC-STORAGE-005・007・009）とBEHAVIOR-RPC-002のバケットのケースも扱います。正本は[英語の契約](../en/slices/grpc-v2-buckets.md)、状態の正本は`compatibility/features.json`です。

`grpc_listen`を設定したサーバーへ`STORAGE_EMULATOR_HOST_GRPC`を向けると、通常のGo `storage.NewGRPCClient(ctx)`でバケットの作成・取得・一覧・削除を利用できます。JSONと同じカタログを使うため、相互に作成したバケットが見えます。新しい設定はありません。

CreateBucketの保持・soft delete・PAP・CORS・Autoclassは共有の型付き検証を再利用します。
作成全体の共有型は未整備のため、残りの作成項目にはJSON insertパーサーを使い、PARTIALを維持します。UpdateBucketは共有更新型を直接作ります。

名前は`projects/_/buckets/NAME`です。作成時のparentは空または`projects/_`、所有projectは`bucket.project`に`projects/<id>`で指定します。全Storage v2 RPCの共通入口でrouting metadataを検査し、ストリーミングは最初のメッセージ、rewrite継続はtokenの名前と照合します。`bucket`・`project`・rewriteの`source_bucket`とRapidの`appendable`を検査し、GoのDirectPath診断を受け付けます。未対応の意味を持つキーはUNIMPLEMENTEDです。バケット名はbucket・object両方のRPCで設定済みの互換プロファイルに従います。不正な名前や食い違う値へのstatusは、SPEC-GRPC-ROUTING-NAMESに記録したローカルな選択です。GoとRustのgRPC SDKでの確認は残っています。

作成時はlocation、storage class、labels、CORS、versioning、default event hold、retention、soft delete、IAM設定の共有JSON検証を使います。保持期間の小数部は、protoのとおり秒単位に切り捨てます（UpdateBucketと同じです）。1秒未満の期間は期間なしとなり、INVALID_ARGUMENTを返します。protoがOUTPUT_ONLYとするフィールド（`bucket_id`・`metageneration`・`location_type`・`create_time`・`update_time`・`owner`とAutoclassの`toggle_time`・`terminal_storage_class_update_time`）は、`google.api.field_behavior`の定めどおり無視し、サーバーが決めた値を返します。Autoclassは[共有の設定規則（英語）](../en/slices/bucket-autoclass-storage-class.md)に従います。object retentionは`enable_object_retention`で有効にします（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。read maskでは`etag`・`autoclass`・`lifecycle`・`object_retention`も選べます。未対応の意味を持つ設定・read maskは`UNIMPLEMENTED`です。取得・削除のメタ世代条件、prefixとページング、非空バケットの削除拒否に対応します。strictでは共有認可を使い、保護されたバケットへの未認証アクセスは`UNAUTHENTICATED`、登録済みprincipalの権限不足は`PERMISSION_DENIED`になります。`allUsers`で許可されたGet/DeleteはJSONと同様に匿名でも成功し、公開アクセス防止も共有の判定に従います。

課金先を指定する`x-goog-user-project`は、2026-11-13から全Storage v2 RPCで無視します（空値も含みます）。JSONの`userProject`も同じく無視します（SPEC-SDK-UNSUPPORTED）。gcs-mimicのバケットではRequester Paysを有効にできないため、課金先が結果を変えることはありません。

projectはIDでも、バケットが返す番号でも指定でき、ListBucketsのページトークンはどちらの書き方でも続きます（BEHAVIOR-COMMON-014）。Storage v1とオブジェクトの転送方式間検証は対象外です。残りの作業は下の台帳のケースのとおりです。AUDIT-008はPARTIALを維持します。性能測定・コンテナ・実クラウド検証は対象外です。

2026-10-28に、型検査、gRPCユニット8件、Goの担当・foundationテスト、最終の`RUST_TEST_THREADS=8 just check`と`just check-sdk`が成功しました。HTTPプロファイルのため、既存のGo V2署名HTTPS専用ケースはSKIPです。台帳の6行は確認済みの範囲と未対応を分けてPARTIALとし、英日対応表を再生成しました。

バケットの作成・更新で拒否する設定（`billing`・`custom_placement_config`・`encryption`・`hierarchical_namespace`・`ip_filter`・`logging`・`rpo`・`website`）は保存されないため、`read_mask`で指定でき、未設定として返します。Cloud Storageも、これらを持たないバケットには未設定として答えました（ケース`grpc-bucket-unset-settings-mask`、2026-11-15に記録）。JavaのSDKは既定のバケット読み取りマスクでこれらをすべて指定します。`satisfies_pzs`は意図して拒否し（SPEC-GRPC-UNMODELED-SETTINGS）、入れ子のパスは未対応のためUNIMPLEMENTEDのままです。

バケットのリソースは、2026-11-17に記録したCloud StorageのUpdateBucketの応答全体（ケース`bkt-gaps-legacy-class-region`のステップ`grpc-update-bucket-class-regional`）と、Javaの既定マスクでのGetBucketの応答全体（ケース`grpc-bucket-unset-settings-mask`）に、無視するフィールドなしで一致します。バケットのACLと既定のオブジェクトACLでは、バケットのプロジェクトのチームを指すエントリに、プロジェクトIDで書いた`entity_alt`（`project-owners-{id}`）を付けます。オブジェクトのACLと同じです。ほかのプロジェクト番号のチームには付けません。その番号がどのIDを指すかをgcs-mimicは知らないからです。`create_time`と`update_time`は、記録どおりミリ秒までです。保持と論理削除の有効日時、Autoclassの時刻、均一アクセスのロック時刻も、JSONと同じくミリ秒に切り捨てます（Storage v2では未記録の推定です）。保持期間が0の論理削除ポリシーには、JSONと同じく`effective_time`を付けません。

2026-11-14に、バケットのオプションを`grpc_bucket_options_reject_only_what_is_unsupported`で固定しました。CreateBucketの`predefined_acl`・`predefined_default_object_acl`（バケットは作られない）と、GetBucketの入れ子の`read_mask`は、オプション名を含むErrorInfo付きUNIMPLEMENTEDを返します。実機記録（UTC で 2026-11-13、ケース`grpc-unsupported-options`）で、Cloud StorageはListBucketsの`return_partial_success`を受け付け、`*`と名前を混ぜた`read_mask`をINVALID_ARGUMENTで断ると分かったので、gcs-mimicも同じ応答に揃えました。入れ子の`read_mask`もCloud Storageは受け付けますが、gcs-mimicでは記録済みのローカルな拒否として残します。RPC-STORAGE-002・003・004には成功ケース`grpc_unsupported_option_rejections`として記録し、ほかの未完了ケースは変えていません。

## 台帳のケース

2026-11-18 に、台帳の各行を、確かめられる振る舞いごとの受け入れケースに分けました。実機の Cloud Storage では実行しないため、Google が定めておらず記録もない応答はローカルな選択とし、固定するテストと合わせて scoped difference に記録します。概要は次のとおりです。

- 意図した拒否（SPEC-GRPC-UNMODELED-SETTINGS の scoped difference）: JSON API も保存しない作成時の設定（`billing`・`encryption`・`logging`・`website`・`custom_placement_config`・`hierarchical_namespace`・`ip_filter`・`satisfies_pzs=true`・`DEFAULT`以外の`rpo`。どれも ErrorInfo 付きで拒否し、バケットを作りません。空のメッセージは設定なしとして受け付けますが、空の`website`だけは拒否します）、GetBucket と ListBuckets の`satisfies_pzs`の read mask、SetIamPolicy の条件付きバインディングと audit config、RPC がルーティングしない`x-goog-request-params`のキー（リダイレクトの`routing_token`と未知のキーを含み、リクエストがリソースを名指しする場合も、ストリームの最初のメッセージで届く場合も拒否します）。
- Storage v2 のすべての RPC の行は BEHAVIOR-RPC-001 に依存します。どの RPC もこのルーティングとリクエストのメタデータの検査を通るためです。下の未完了のケースがあるあいだ、各 RPC の行は PARTIAL のままです。
- 記録済みのローカルな選択（特記がなければ SPEC-GRPC-LOCAL-CHOICES）: 小数を含む soft delete の期間の拒否、proto が出力専用としていない作成時のフィールド（空でない`etag`、保持と soft delete の`effective_time`、ロック済みの保持ポリシー、均一アクセスの`lock_time`）の拒否、バケットの各 RPC の metageneration 条件の規則、UpdateBucket の古い ETag や別の ETag への FAILED_PRECONDITION、DeleteBucket のエラーの順序、未確定の appendable オブジェクトだけを持つバケットの削除、Bucket のフィールドを指さない read mask への UNIMPLEMENTED とその検査の順序、不正または食い違うルーティング値への INVALID_ARGUMENT（SPEC-GRPC-ROUTING-NAMES）。
- 実装が必要なもの: 入れ子のバケット read mask（Cloud Storage は受け付けます）、CreateBucket の lifecycle・ACL・predefined ACL。
- テストが必要なもの: ルーティングを確かめる Go と Rust の gRPC SDK。
- 判断が必要なもの: サーバーで`grpc-timeout`を適用するか、gzip のメッセージ圧縮に対応するか。

## 2026-11-18のバケットの変更

応答を変えたのは 5 件です。UpdateBucket の ETag の条件、CreateBucket の保持期間の小数部の切り捨てと出力専用フィールドの扱い、両 RPC での正規形でない`Duration`の保持期間の切り捨て、CreateBucket の小数を含む soft delete の期間への拒否メッセージです。最初の 1 件を除く 4 件は、変更前の応答を既存のテストが固定しており、修正のたびにそのテストをコードと一緒に変えました。ETag を無視していた応答を固定したテストはなく、ETag のテストは修正と同時に追加しました。ほかの変更は応答を変えず、コードを変える前にテストで固定しました。

- UpdateBucket は`bucket.etag`を条件として扱います。proto は、`UpdateBucketRequest`のメタデータに含まれる場合は`etag`が一致するときだけ操作する、と定めています（storage.proto の 2581〜2584 行）。認可の後にバケットを読み、古い ETag や別の ETag には metageneration 条件の失敗と同じ FAILED_PRECONDITION を返します。一致した場合は、その metageneration を明示の一致条件としてカタログに渡し、書き込みと不可分に比較します。バケットの ETag は metageneration だけを表すので、二つの条件は同じ意味です。存在しないバケットは NOT_FOUND のままで、ETag のない更新は変わりません（`grpc_bucket_update_requires_a_matching_etag`）。ETag は明示の条件になるため、ETag 付きの更新は条件のない更新のようには再試行しません。lifecycle の確認から書き込みまでのあいだに別の更新が入ると、すぐに FAILED_PRECONDITION を返し、何も変えません（`grpc_bucket_update_with_an_etag_fails_at_once_when_another_update_intervenes`）。proto は ETag が一致しないときの status を定めていないため、FAILED_PRECONDITION は記録済みのローカルな選択です（SPEC-GRPC-LOCAL-CHOICES）。Google の一般的な設計指針 [AIP-154](https://google.aip.dev/154) は ABORTED を求めていますが、proto はこれを引用しておらず、適用するかはオーナーの判断に残しています。
- CreateBucket は、上のとおり保持期間の小数部を切り捨て、出力専用のフィールドを無視します（`create_bucket_rounds_retention_nanos_down`・`create_bucket_ignores_output_only_fields`）。UpdateBucket の切り捨ては`grpc_bucket_update_rounds_retention_nanos_down`で固定しました。
- 両 RPC は保持期間を正規化してから切り捨てます。そのため、nanos の符号が seconds と逆の`Duration`や、nanos が 1 秒以上ある`Duration`も、値どおりに切り捨てます。3600 秒と -1 ナノ秒は 3599、0 秒と 1500000000 ナノ秒は 1 を保存し、1 秒と -1 ナノ秒は 1 秒未満なので INVALID_ARGUMENT を返します。以前は nanos を捨てていたため、順に 3600 を保存、拒否、1 を保存していました（上の二つの切り捨てのテストで固定しています）。
- CreateBucket は小数を含む soft delete の期間を、共有のヘルパーで UpdateBucket と同じメッセージ`fractional soft delete duration; use whole seconds`で拒否します。以前のメッセージは、切り捨てで受け付ける保持期間の名前を示していました（`create_rejects_output_fields_and_fractional_durations_in_order`）。
- UpdateBucket は`update_mask`の各パスを型付きの`UpdatePath`に一度だけ解析し、mask の順に適用します。応答は変わらず、`update_mask_paths_select_their_settings_and_refuse_the_rest`で固定しています。受け付けるパスは自分の設定だけを変え、それ以外のパス（空のラベルキー、大文字小文字の違うつづり、受け付けるフィールドの下位パス、出力専用のフィールドを含みます）はパスを示す UNIMPLEMENTED を返します。
- バケットの metageneration 条件は、オーナーの判断で RPC ごとの規則を保ちます。GetBucket と DeleteBucket は届いた値をそのまま比較し（0 や負の一致条件は成り立たず、0 や負の不一致条件は常に成り立ちます）、UpdateBucket は 0・負の値・両条件の同時指定を拒否します。LockBucketRetentionPolicy は proto が正の値を必須としているため 0 と負の値を拒否します。拒否はどれもバケットを探す前です（`bucket_rpcs_keep_their_own_metageneration_condition_rules`）。
- DeleteBucket は、認可、バケットの有無、条件、残っているオブジェクトの順に答えます（`delete_bucket_reports_authorization_existence_conditions_then_contents`）。非現行の世代はバケットを残し、論理削除済みのオブジェクト・未完了の XML multipart upload・未確定の appendable オブジェクトは残しません。soft delete ポリシーを持つバケットは論理削除済みの版として残ります（`delete_bucket_follows_the_shared_catalog_for_retained_state`）。
- 新たに固定した拒否: GetBucket と ListBuckets の`satisfies_pzs`・未知・入れ子の read mask とその順序（`bucket_read_masks_refuse_unselectable_paths_in_each_rpcs_order`）、SetIamPolicy の条件付きバインディングと audit config（`grpc_iam_set_refuses_conditional_bindings_and_audit_configs`）、CreateBucket の空の`website`、バケットを探す前に`storage.buckets.update`を求める strict の LockBucketRetentionPolicy（`grpc_retention_lock_requires_bucket_update_permission`）。

2026-11-18 に、最終のソースで`cargo test --locked --offline --lib grpc::`（264 件）と`bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh`（16 のテストバイナリと doctest。ライブラリのテストは 2468 件）が成功しました。`cargo fmt --all --check`、clippy、`cargo xtask lint`、台帳の検証、`bash scripts/check-docs.sh`、`bash scripts/check-markdown.sh`も成功しました。SDK・差分の再生・実クラウドでの確認はしていません。

これらの行でまだ残っているもの: 入れ子のバケット read mask、CreateBucket の lifecycle・ACL・predefined ACL、UpdateBucket の`*`の mask と predefined ACL、ポリシーの一部のフィールドだけを指定する SetIamPolicy の mask、オブジェクトへの TestIamPermissions、出力専用の更新パスを UpdateBucket と UpdateObject でそろえて無視するかの判断、UpdateBucket での`storage.buckets.setIamPolicy`の個別の検査（`storage.buckets.update`を与えるロールはどれも`storage.buckets.setIamPolicy`も与えるため、今は登録できる呼び出し元では確かめられません）。
