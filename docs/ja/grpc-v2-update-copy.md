# Storage v2の更新・コピー

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-STORAGE-005〜010、012、017、021、024とBEHAVIOR-RPC-002です。正本は[英語の契約](../en/slices/grpc-v2-update-copy.md)です。全行をPARTIALとし、未対応範囲を台帳に残します。

フィールドを含む検証エラーは型付きの理由を持ち、JSONとgRPCでそれぞれのフィールド名を使います。
エラーに含めるクライアントの入力値は変更しません。UpdateObjectのmaskに`metadata`と`metadata.KEY`を
混在させると、どちらかを選ぶよう案内してINVALID_ARGUMENTを返します。
未対応のIAMロール・メンバー種別には、対応するポリシー入力を案内します。
convenience memberとJSONクエリのプロジェクトIDは、同じ検証処理を使います。

UpdateBucket・UpdateObject・TestIamPermissionsの再現精度は、共有するJSONの行と同じLOCAL_SIMULATIONです。ほかの実装済み範囲はEXACTですが、未対応のケースを残したPARTIALであり、API全体の一致は主張しません。

バケット・オブジェクトのfield mask更新、バケットIAMの取得・保存・権限検査、保持ポリシーのロック、rewrite・compose・同一バケット内の原子的なmove・soft delete世代のrestoreを、JSONと同じカタログ・認可・検証で処理します。JSONで用意したデータへのgRPC更新を、JSONで確認できます。本文上限は既存設定を使い、共有処理が64 KiBのバッファで転送します。新しい設定や依存は追加しません。

対応するmaskは、labels・versioning・CORS・lifecycle・保持・soft delete・UBLAなど、JSONで対応済みの項目に限ります。バケットでは`acl`・`default_object_acl`も更新できます。オブジェクトではmetadata・固定メタデータ・custom time・holdに加え、`acl`（オブジェクトACLの置換）と`retention`（object retentionの設定・削除）を更新できます（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。指定しない項目は保持します。空または未指定のupdate maskと、フィールドを指さないパスは、Cloud Storageの文言のINVALID_ARGUMENTです（`There is no field for this resource named '<path>'.`、空のmaskは理由`GRPC_MISSING_UPDATE_MASK_FIELDS`の`Invalid update_mask. Requests should specify at least one update field.`。いずれも2026-11-18に記録）。出力専用のパスは受け付けて何も変えません（`create_time`と`update_time`は記録済みで、ほかのOUTPUT_ONLYのフィールドは推定です）。オブジェクトでは、この場合もmetagenerationだけが進みます。Go SDKは空の更新で出力専用の`create_time`を送ります。UpdateBucketの`*`は、Cloud Storageが全設定の置換として受け付けたのに対しUNIMPLEMENTEDで、宣言済みのローカルな選択です（SPEC-GRPC-LOCAL-CHOICES）。UpdateObjectの`*`は、JSONのPUTと同じく編集可能なメタデータを置き換えます。Cloud Storageは、content typeだけを持つ`*`の更新で、content language・cache control・カスタムメタデータを消し、ACLを残しました（記録済み。`a_wildcard_mask_replaces_the_editable_metadata_and_keeps_the_acl`）。リクエストに`acl`や`retention`もあれば、それぞれのパスと同じに適用し、リクエストが設定しないholdも解除します（後者2点は記録がありません）。`*`をほかのパスと並べた場合、`contexts`、`storage_class`、そのほかの未対応maskはErrorInfo付きのUNIMPLEMENTEDです。UpdateBucketの`soft_delete_policy.retention_duration`が小数を含むと、INVALID_ARGUMENT`Invalid argument.`と`BadRequest`を返します（記録済み）。`bucket.etag`は無視します。Cloud Storageは古いETagを指定した更新も適用したため、条件になるのはリクエストのmetageneration条件だけです。`metadata` maskは、リクエストのエントリを保存済みのマップへマージし、エントリがなければマップを空にします。`metadata.`は空のキーを指します。Cloud Storageは2026-11-17にどちらもそう動作しました（ケース`obj-gaps-empty-metadata-key`）。固定したGo SDKは、全キーを消すときだけ空のマップでこのmaskを送ります。`labels` maskはバケットのラベル全体を置き換えますが、記録はありません。これらの変更は共有の更新トランザクション内で行い、暗黙の前提条件は追加せず、クライアントが明示した条件だけを適用します。キー単位の更新は指定したキーだけをマージします。

UpdateObjectは、objectに指定した`etag`を無視します。固定したprotoは`Object.etag`について「update・deleteのリクエストのメタデータに含めると、etagがライブオブジェクトのものと一致する場合だけ操作を行う」と定めています（2902〜2906行）。しかしCloud Storageは、古くなったetagを指定した更新も現在のetagを指定した更新も同じように適用しました（2026-11-18に記録）。そのため、更新の条件になるのはリクエストの世代・metagenerationの条件だけです（`update_object_ignores_the_request_etag`）。

iam_configの親maskでは、2つの設定の扱いが異なります。省略したPAPはinheritedに戻り、UBLAはそのメッセージがあるときだけ変わります。Cloud Storageは、空の`iam_config`でPAPをinheritedに戻してmetagenerationを進め（ケース`grpc-empty-iam-update`）、UBLAだけの`iam_config`でUBLAを有効にしてPAPをinheritedに戻し、PAPだけの`iam_config`では有効なUBLAを保ちました（ケース`grpc-iam-config-partial-update`）。いずれも2026-11-13の記録です。Go Storage 1.70.0は個別の設定でも親maskを送るため、UBLAだけを指定するとPAPも戻り、PAPだけを指定してもUBLAは保たれます。`iam_config.public_access_prevention`などの末端maskは、その設定だけを変えます。versioning・保持・soft delete・lifecycleなどの親maskも確認し、単一の編集項目やリストの明示的な解除・置換は維持します。

UpdateBucketはprotoから共有の`BucketMetadataUpdate`を直接作り、JSON本文の組み立てと再解析を行いません。
UpdateObjectも`ObjectMetadataUpdate`を直接作り、ACL・保持時刻を型付きで渡します。
WriteObject・ComposeObject・RewriteObjectはprotoのメタデータ変換と共有検証を再利用します。
rewrite tokenの照合用保存形式は既存互換を保ちますが、適用時のJSONパーサーは通りません。
SetIamPolicyは共有の型付きポリシーを直接作ります。TestIamPermissionsは渡された権限文字列をそのまま検証し、クエリ文字列を組み立てません。
権限の重複は入力順に除き、入力サイズはgRPCメッセージ上限で制限します。権限ごとのカタログ呼び出しは残ります。
空の`iam_config`親maskも更新としてmetagenerationを進め、PAPをinheritedに戻し、UBLAは保ちます。
この動作は2026-11-13の記録で確認し、SPEC-GRPC-EMPTY-IAM-UPDATEは解決済みです。

バケットの共有検証で返るAutoclass・lifecycle・保持エラーも、gRPCではprotoのフィールド名で案内します。
更新トランザクション内で検出するエラーも対象です。JSONの文言は維持し、gRPCの保持期間の案内にはJSON本文の例を入れません。

rewriteは共有の継続セッションを使い、token単独での継続と直前tokenの再送に対応します。tokenはgRPC内で使い続けてください。JSONとのtoken共用は対象外です。composeは1〜32個で世代・前提条件を検査します。ComposeObjectとRewriteObjectは`object_checksums`を書き込む予定のオブジェクトと照合します（後述の2026-11-17の記載を参照）。KMS鍵・条件付きIAM・宛先の`retention`、エントリを列挙した宛先の`acl`などは未対応です（UNIMPLEMENTED）。ComposeObjectの宛先の`storage_class`は、ソースのクラスと同じなら受け付け、異なる場合はINVALID_ARGUMENTと`BadRequest`の`Cannot compose object with destination storage class NEARLINE from source components that have different storage classes (STANDARD).`で拒否します。サービスだけが設定するフィールド（generation・metageneration・size・作成・更新時刻とほかのOUTPUT_ONLYのフィールド）は無視します（ケースで名指ししたものは記録済み、残りは推定です。`compose_object_accepts_the_sources_class_and_ignores_service_set_fields`）。RewriteObjectの宛先がサービス設定のフィールドを持つ場合は、INVALID_ARGUMENT`You must specify an object name.`と`BadRequest`で拒否します（generation・metageneration・size・両時刻をまとめて送った記録があります）。`acl`のエントリを列挙した宛先はUNIMPLEMENTEDです。公開時に新しい世代へ既定または定義済みのACLを付けるためで、Cloud Storageは両方を適用したので、この2つのACLの拒否は宣言済みのローカルな選択です（SPEC-GRPC-LOCAL-CHOICES）。lifecycleは検証・保存の範囲であり、実行処理は追加しません。

世代とmetagenerationの条件は、届いた値をそのまま比べます。Cloud Storageの応答に合わせたものです（ケース`grpc-probe-object-conditions`、2026-11-18に記録）。UpdateObject・ComposeObject（宛先が取るのはmatchの2条件だけです）・RewriteObject（宛先とソース）・RestoreObjectは、どのオブジェクトも持たないmetagenerationのmatch 0を、FAILED_PRECONDITION`At least one of the pre-conditions you specified did not hold.`として評価します。matchとnot-matchの同時指定は1組として評価し、現在の値を指すnot-matchは、rewriteのソースも含めて、FAILED_PRECONDITION`The resource you requested has not been modified according to the condition you specified.`です。soft deleteポリシーのないバケットでのRestoreObjectは、条件を読む前に`bucket soft delete policy must be set.`のBadRequestを返します。MoveObjectはmetagenerationのmatch 0を比べますが、宛先でもソースでもmatchとnot-matchの同時指定は、INVALID_ARGUMENTと`BadRequest`の`Found conflicting object preconditions. Mutually exclusive fields cannot be set simultaneously.`で拒否します。記録したのは同じ値を指す組だけで、ほかの組も形だけで拒否するのは推定です。オブジェクトのRPCでは、負の世代・metagenerationの条件を、今もINVALID_ARGUMENT`Preconditions must be nonnegative; use if_generation_match=0 to create only`で拒否します（オブジェクトでは記録がありません。`move_object_refuses_condition_pairs_and_failing_conditions_without_moving`、`update_object_refuses_malformed_and_unmodeled_requests_without_changing_the_object`）。LockBucketRetentionPolicyは正の`if_metageneration_match`を求める厳格な規則を保ち、記録はありません。JSONの複合条件の優先順位までは、この範囲では再現しません。

ComposeObject・RewriteObject・MoveObject・RestoreObjectの書き込み結果も共通のowner・ACL表示処理を使います。
ACL読み取り権限がない場合とUBLAでは、owner・ACLを返しません。
これらの派生RPCはWriteObject・UpdateObjectの観測からの推定で、Googleでは未観測です。
SPEC-GRPC-DERIVED-OWNER-VIEWに記録し、保存する完了応答は呼び出し元に依存させず、返却時に現在の権限を適用します。

Go Storage 1.70.0の既定の高水準APIを使い、permissiveとstrictを検証します。型検査後、Rustでmask・世代条件・保持ロック・分割rewriteの再送・32/33個の境界を検証します。コンテナ・ガイド・性能測定・実Google Cloudとの比較は実施しません。最終検証結果は英語版に記録します。

IAM設定の保持については、修正前にRustとGoの両方で失敗を再現しました。最終ソースで担当Rustテスト5件とjust check-sdkが成功し、Goの更新・strict・foundationも確認しています。Storage v2はストレージのドメインエラーをJSONアダプターを通さずに直接statusへ対応づけます（`src/grpc/v2/status.rs`）。期限切れのrewriteセッションはFAILED_PRECONDITION、未対応の設定はErrorInfo付きUNIMPLEMENTEDです。未対応呼び出しの確認にはRapidの書き込みを使います。CancelResumableWriteは[オブジェクト入出力](grpc-v2-object-io.md)の契約で実装しています。HTTP実行のため、既存のGo V2署名テスト1件だけスキップしました。

bucket・object I/Oを統合した最終ソースでjust checkも成功しました。ライブラリ528件、全統合テスト、native・Windows・Linuxのclippy、台帳591行と生成表の整合性を確認済みです。IAM設定の保持の問題は解消し、ほかの親maskは単一項目の解除・置換として正しいため変更しません。

2026-11-14に、テストのなかった未対応オプションの拒否を3つのテストで固定しました。どのオプションも、状態を変える前に、オプション名を含むErrorInfo付きUNIMPLEMENTEDを返します。UpdateBucket・UpdateObjectのpredefined ACLはメタデータとmetagenerationを変えません。ComposeObjectの`kms_key`とソースと異なる宛先のストレージクラス（記録済みのBadRequest）、RewriteObjectの`destination_kms_key`、RestoreObjectの`restore_token`・顧客指定の暗号鍵は、宛先オブジェクトを作りません。ComposeObject・RewriteObject・UpdateObjectの顧客指定の暗号鍵は[gRPC の CSEK](grpc-v2-customer-supplied-keys.md)のとおりに扱います。SetIamPolicyの`bindings`と`etag`以外のmaskと、managed folder・objectのIAMリソースはポリシーを変えません。`cargo test --locked --offline --lib grpc::`の168件が成功しました。RPC-STORAGE-006・007・009・010・012・017・021には成功ケース`grpc_unsupported_option_rejections`として記録し、`grpc_full_contract`は残りのフィールド・エラーの組み合わせ・実機との一致のためPENDINGのままです。2026-11-18に、`grpc_full_contract`を個別のケースに分けました（下の台帳のケースを参照）。

これらのテストが固定しているのはgcs-mimic自身の拒否で、Cloud Storageも同じ要求を断ることを示すものではありません。実機記録（UTC で 2026-11-13、ケース`grpc-unsupported-options`）では、Cloud StorageはpredefinedのACL、UpdateObjectの`*`マスク、派生書き込みの`custom_time`、ポリシーのバージョン2、オブジェクトへのTestIamPermissionsを受け付けました。誤ったchecksumはINVALID_ARGUMENT、uniform accessのないバケットのmanaged folder IAMはFAILED_PRECONDITIONで断りました。gcs-mimicはポリシーのバージョン2を受け付けるように変え、ほかの拒否はSPEC-SDK-UNSUPPORTEDの記録済みローカル選択として残します。また、ComposeObjectとMoveObjectの応答には`acl`と`owner`が含まれず、RewriteObjectには含まれると分かったので、gcs-mimicもこの2つのRPCでは省きます。

2026-11-17に、UpdateObject・ComposeObject・RewriteObjectのpredefined ACLと、ComposeObject・RewriteObjectの`temporary_hold`・`custom_time`を、JSONと同じカタログの経路で受け付けるようにしました。UpdateObjectはACLを置き換えます。同じ要求で`acl`も更新する場合は拒否します。ComposeObjectとRewriteObjectは公開時にpredefined ACLを適用し、匿名の呼び出し元からの指定は拒否します。rewriteのtokenは`destinationPredefinedAcl`も束縛します。ComposeObjectは、predefined ACLを指定したときだけ`acl`と`owner`を返します。実機記録のCloud Storageと同じ動きです。UpdateBucketのpredefined ACL、KMS鍵、宛先の`retention`は引き続き拒否します。ComposeObjectの宛先の`storage_class`は、全ソースがそのクラスなら受け付けます。

同じ2026-11-17に、ComposeObjectとRewriteObjectのリクエストのchecksumを、新しい実機記録（ケース`grpc-gaps-derived-checksums`、[実機記録の証跡（英語）](../en/evidence/provider-differential-gap-probes.md#sixth-recording-storage-v2-request-checksums-on-derived-writes)）に合わせ、誤ったchecksumの記録済みローカル選択を2件外しました。ComposeObjectは、指定されたCRC32Cを、ソースから合成したCRC32Cとバイトを読む前に照合します。合成オブジェクトにはMD5がないため、MD5の指定は値によらずINVALID_ARGUMENT`Invalid argument.`です。RewriteObjectは、16バイトでないMD5を`Provided value (<base64>) is not a base64-encoded 128-bit MD5 hash.`で拒否してから、指定されたCRC32CとMD5をセッション開始前にソースの保存済みハッシュと照合します。tokenはこれらを`destinationPredefinedAcl`の後に束縛するため、別のchecksumを指定した継続はtokenの不一致として拒否します。不一致はCRC32Cを先に調べ、Cloud StorageのCRC32CまたはMD5のメッセージと、それを繰り返す`BadRequest`を付けたINVALID_ARGUMENTを返し、何も書き込みません。合成オブジェクトのように、MD5を示さないソースのrewriteでは、composeと同じくMD5の指定を`Invalid argument.`で拒否します（推定）。JSONのrewriteは指定された`md5Hash`を無視しますが、Storage v2のrewriteはCloud Storageと同じく検証します。ソースに記録されたハッシュとコピーしたバイトが合わない場合は、保存データの破損（UNAVAILABLE）として報告し、クライアントが指定したchecksumの誤りとは扱いません。`grpc_derived_writes_verify_request_checksums`、`grpc_rewrite_tokens_bind_declared_checksums`、`rewrite_tokens_bind_the_stored_parameter_spelling`がこの動作を固定しています。なお、このケースで完了したrewriteの応答には、ソースの`contentType`がありませんでした。gcs-mimicはこれをコピーします。Storage v2がいつ`contentType`を落とすかは未解決です。

2026-11-18 に、ケース`grpc-probe-object-masks`の記録で、Cloud Storage は古くなった`etag`を指定した UpdateObject も適用すると分かりました。そのため UpdateObject はリクエストの etag を無視します。`src/grpc/v2/update_object/object_etag.rs`の`update_object_ignores_the_request_etag`がこれを固定しています。現在の etag、古い etag、でたらめな etag、etag なしのどれでも更新を適用し、metageneration を進めます。以前の、一致しない etag を FAILED_PRECONDITION で拒否して選択した世代の etag と比べる動作は、そのテストごと削除しました。この変更では SDK のテストも実機への要求も実行していません。

同じ2026-11-18に、テストのなかったUpdateObject・ComposeObject・RewriteObject・RestoreObjectの拒否をテストで固定しました。下の応答を固定するテストで、記録のあるものは2026-11-18のCloud Storageの記録に合わせています。どの拒否も保存済みのオブジェクトを変えません。

- `update_object_refuses_malformed_and_unmodeled_requests_without_changing_the_object`: objectやupdate_maskの欠落、負のgeneration、負の条件、modeまたは`retain_until_time`のないretentionはINVALID_ARGUMENTです。metagenerationのmatch 0とmatchにnot-matchを加えた指定は、Cloud Storageと同じFAILED_PRECONDITIONです。オブジェクトのフィールドを指さないパスはINVALID_ARGUMENTで、`contexts`・`contexts.custom`・`storage_class`のパスと、ほかのパスと並べた`*`はErrorInfo付きのUNIMPLEMENTEDです。
- `compose_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination`: 宛先の欠落、負のソース世代・ソースの条件はINVALID_ARGUMENTです。宛先のretention・KMS鍵・明示的なACLはUNIMPLEMENTEDです。metagenerationのmatch 0はFAILED_PRECONDITION、ソースと異なる宛先のストレージクラスは`BadRequest`付きのINVALID_ARGUMENTで、どちらも記録どおりです。
- `rewrite_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination`: 負のソース世代、負のソース条件、負または1 MiBの倍数でない転送量、bucketやnameを含む宛先リソース、不正なtoken、サービス設定のフィールドを持つ宛先はINVALID_ARGUMENTです。metagenerationのmatch 0と、ソースのmatchにnot-matchを加えた指定は、記録どおりFAILED_PRECONDITIONです。宛先リソースのKMS鍵はUNIMPLEMENTEDです。
- `restore_object_refuses_a_missing_generation_and_copying_the_source_acl`: generationの欠落や負の値はINVALID_ARGUMENTです。soft deleteしたソースに対する`copy_source_acl`のtrueと`restore_token`はUNIMPLEMENTEDで、何も復元しません。`copy_source_acl`のfalseは復元します。

UpdateBucketの`*`のmask、宛先のretentionとACL、`copy_source_acl`は、下の台帳のケースのとおり実装が必要なものとして残ります。

## 台帳のケース

2026-11-18 に、台帳の各行を、確かめられる振る舞いごとの受け入れケースに分けました。2026-11-18 に Cloud Storage から記録した応答は、合わせた動作です。Google が定めておらず記録もない応答はローカルな選択とし、固定するテストと合わせて scoped difference に記録します。概要は次のとおりです。

- 意図した拒否（SPEC-GRPC-UNMODELED-SETTINGS の scoped difference）: JSON API も保存しない設定（`rpo`・`website`・`logging`・`encryption`・`billing`・`custom_placement_config`・`hierarchical_namespace`・`ip_filter`・`satisfies_pzs`）への UpdateBucket の mask（それぞれ ErrorInfo まで固定しています）、UpdateObject の`contexts`と`contexts.custom`の mask、ComposeObject の`kms_key`と RewriteObject の`destination_kms_key`、GetIamPolicy・SetIamPolicy・TestIamPermissions での managed folder の IAM リソース、階層型名前空間のバケットだけが使う RestoreObject の`restore_token`。
- 宣言済みのローカルな選択（Cloud Storage は 2026-11-18 の記録で異なる応答をしました。SPEC-GRPC-LOCAL-CHOICES）: UpdateBucket の`*`の update mask と入れ子のバケット read mask は UNIMPLEMENTED で拒否します。ComposeObject と RewriteObject の宛先の`acl`も UNIMPLEMENTED で拒否します。GetIamPolicy のオブジェクトリソースへの UNIMPLEMENTED は、Cloud Storage がオブジェクトの ACL をポリシーとして描画するのに対し、gcs-mimic はオブジェクトのポリシーを持たないためです。
- そのほかのローカルな選択（SPEC-GRPC-LOCAL-CHOICES）: SetIamPolicy のオブジェクトリソースへの UNIMPLEMENTED（proto はバケットと managed folder だけを挙げています）、古いポリシーの etag と、発行する etag の 8 バイトに合わない不正な etag への FAILED_PRECONDITION、記録済みの UpdateBucket の応答と同じヘルパーを使う CreateBucket の小数を含む soft delete の期間への応答、リクエストが metageneration の条件を持たない場合に UpdateBucket の暗黙の lifecycle 確認が 3 回競合した後に返す ABORTED、proto が定めておらず JSON の更新でも適用しない`storage_class`の update mask を UpdateObject が拒否すること（JSON アダプターは変更された`storageClass`を未対応の設定として拒否し、同じ値の繰り返しだけを受け付けます）。ポリシーのないバケットのロックと 2 回目のロックへの INVALID_ARGUMENT（SPEC-RETENTION-POLICY）。RestoreObject の顧客指定の暗号鍵の拒否（SPEC-GRPC-CSEK）と、owner・ACL を付けた応答（SPEC-GRPC-DERIVED-OWNER-VIEW）。
- 実装が必要なもの: UpdateBucket の`*`の update mask、Cloud Storage と JSON の PATCH が適用する UpdateBucket の predefined ACL、ポリシーの一部のフィールドだけを指定する SetIamPolicy の mask、proto が定め Cloud Storage が受け付けるオブジェクトへの TestIamPermissions、ComposeObject・RewriteObject の宛先の`retention`と`acl`、RestoreObject の`copy_source_acl`。
- テストが必要なもの: PAP と ACL の変更で別に確かめる`storage.buckets.setIamPolicy`、ComposeObject のソースの`storage.objects.get`と上書き時の`storage.objects.delete`、ACL 読み取り権限のない呼び出し元への predefined ACL 付き ComposeObject の応答、もう一方の転送方式で使う rewrite token。
- 判断が必要なもの: rewrite したオブジェクトにソースの content type を残すか（記録した Cloud Storage の完了応答にはありませんでした）、MoveObject の権限（JSON の move と同じく`storage.objects.get`・`.delete`・`.create`を確かめますが、proto は`storage.objects.move`・`.create`と、上書き時だけの`.delete`を挙げています）。バケットとオブジェクトの条件は記録した Cloud Storage の応答に従い、LockBucketRetentionPolicy と負のオブジェクト条件だけがローカルな拒否を保ちます。

GetIamPolicy（RPC-STORAGE-006）自体には未完了のケースがありません。ただし Storage v2 のほかの行と同じく BEHAVIOR-RPC-001 に依存します。どの RPC も、そのルーティングとリクエストのメタデータの処理を通るためです。このため、BEHAVIOR-RPC-001 のルーティングを確かめる SDK、サーバー側の`grpc-timeout`、gzip のメッセージ圧縮が未完了のあいだは PARTIAL のままです。
