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

対応するmaskは、labels・versioning・CORS・lifecycle・保持・soft delete・UBLAなど、JSONで対応済みの項目に限ります。バケットでは`acl`・`default_object_acl`も更新できます。オブジェクトではmetadata・固定メタデータ・custom time・holdに加え、`acl`（オブジェクトACLの置換）と`retention`（object retentionの設定・削除）を更新できます（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。指定しない項目は保持し、未対応maskはErrorInfo付きのUNIMPLEMENTEDです。Go SDKが空の更新で送る出力専用のcreate_timeは無視します。`metadata` maskは、リクエストのエントリを保存済みのマップへマージし、エントリがなければマップを空にします。`metadata.`は空のキーを指します。Cloud Storageは2026-11-17にどちらもそう動作しました（ケース`obj-gaps-empty-metadata-key`）。固定したGo SDKは、全キーを消すときだけ空のマップでこのmaskを送ります。`labels` maskはバケットのラベル全体を置き換えますが、記録はありません。これらの変更は共有の更新トランザクション内で行い、暗黙の前提条件は追加せず、クライアントが明示した条件だけを適用します。キー単位の更新は指定したキーだけをマージします。

UpdateObjectのobjectに指定した`etag`は条件として扱います。固定したprotoは`Object.etag`について「update・deleteのリクエストのメタデータに含めると、etagがライブオブジェクトのものと一致する場合だけ操作を行う」と定めています（2902〜2906行）。空でないetagは、更新トランザクションのなかで、世代・metagenerationの条件の後、顧客指定の暗号鍵の前に、更新する世代のetagと完全一致で比べます。一致しなければ何も変えず、世代・metagenerationの条件の失敗と同じメッセージのFAILED_PRECONDITIONを返します。空のetagは条件になりません。protoは不一致のstatusを定めておらず、ライブオブジェクトのことしか述べていないため、statusと比べる世代は記録済みのローカルな選択です（SPEC-GRPC-LOCAL-CHOICES）。

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

rewriteは共有の継続セッションを使い、token単独での継続と直前tokenの再送に対応します。tokenはgRPC内で使い続けてください。JSONとのtoken共用は対象外です。composeは1〜32個で世代・前提条件を検査します。ComposeObjectとRewriteObjectは`object_checksums`を書き込む予定のオブジェクトと照合します（後述の2026-11-17の記載を参照）。KMS鍵・条件付きIAM・派生オブジェクトの`storage_class`と`retention`などは未対応です。lifecycleは検証・保存の範囲であり、実行処理は追加しません。

同じ種類の世代にmatchとnot-matchを同時に指定するとINVALID_ARGUMENTです。JSONの複合条件の優先順位までは、この範囲では再現しません。

ComposeObject・RewriteObject・MoveObject・RestoreObjectの書き込み結果も共通のowner・ACL表示処理を使います。
ACL読み取り権限がない場合とUBLAでは、owner・ACLを返しません。
これらの派生RPCはWriteObject・UpdateObjectの観測からの推定で、Googleでは未観測です。
SPEC-GRPC-DERIVED-OWNER-VIEWに記録し、保存する完了応答は呼び出し元に依存させず、返却時に現在の権限を適用します。

Go Storage 1.70.0の既定の高水準APIを使い、permissiveとstrictを検証します。型検査後、Rustでmask・世代条件・保持ロック・分割rewriteの再送・32/33個の境界を検証します。コンテナ・ガイド・性能測定・実Google Cloudとの比較は実施しません。最終検証結果は英語版に記録します。

IAM設定の保持については、修正前にRustとGoの両方で失敗を再現しました。最終ソースで担当Rustテスト5件とjust check-sdkが成功し、Goの更新・strict・foundationも確認しています。共通のstatus::from_jsonを使い、410はFAILED_PRECONDITION、501はErrorInfo付きUNIMPLEMENTEDです。未対応呼び出しの確認にはRapidの書き込みを使います。CancelResumableWriteは[オブジェクト入出力](grpc-v2-object-io.md)の契約で実装しています。HTTP実行のため、既存のGo V2署名テスト1件だけスキップしました。

bucket・object I/Oを統合した最終ソースでjust checkも成功しました。ライブラリ528件、全統合テスト、native・Windows・Linuxのclippy、台帳591行と生成表の整合性を確認済みです。IAM設定の保持の問題は解消し、ほかの親maskは単一項目の解除・置換として正しいため変更しません。

2026-11-14に、テストのなかった未対応オプションの拒否を3つのテストで固定しました。どのオプションも、状態を変える前に、オプション名を含むErrorInfo付きUNIMPLEMENTEDを返します。UpdateBucket・UpdateObjectのpredefined ACLはメタデータとmetagenerationを変えません。ComposeObject・RewriteObjectのKMS鍵・ACL・checksum・`storage_class`・`temporary_hold`・`custom_time`と、RestoreObjectの`restore_token`・顧客指定の暗号鍵は、宛先オブジェクトを作りません。ComposeObject・RewriteObject・UpdateObjectの顧客指定の暗号鍵は[gRPC の CSEK](grpc-v2-customer-supplied-keys.md)のとおりに扱います。SetIamPolicyの`bindings`と`etag`以外のmaskと、managed folder・objectのIAMリソースはポリシーを変えません。`cargo test --locked --offline --lib grpc::`の168件が成功しました。RPC-STORAGE-006・007・009・010・012・017・021には成功ケース`grpc_unsupported_option_rejections`として記録し、`grpc_full_contract`は残りのフィールド・エラーの組み合わせ・実機との一致のためPENDINGのままです。2026-11-18に、`grpc_full_contract`を個別のケースに分けました（下の台帳のケースを参照）。

これらのテストが固定しているのはgcs-mimic自身の拒否で、Cloud Storageも同じ要求を断ることを示すものではありません。実機記録（UTC で 2026-11-13、ケース`grpc-unsupported-options`）では、Cloud StorageはpredefinedのACL、UpdateObjectの`*`マスク、派生書き込みの`custom_time`、ポリシーのバージョン2、オブジェクトへのTestIamPermissionsを受け付けました。誤ったchecksumはINVALID_ARGUMENT、uniform accessのないバケットのmanaged folder IAMはFAILED_PRECONDITIONで断りました。gcs-mimicはポリシーのバージョン2を受け付けるように変え、ほかの拒否はSPEC-SDK-UNSUPPORTEDの記録済みローカル選択として残します。また、ComposeObjectとMoveObjectの応答には`acl`と`owner`が含まれず、RewriteObjectには含まれると分かったので、gcs-mimicもこの2つのRPCでは省きます。

2026-11-17に、UpdateObject・ComposeObject・RewriteObjectのpredefined ACLと、ComposeObject・RewriteObjectの`temporary_hold`・`custom_time`を、JSONと同じカタログの経路で受け付けるようにしました。UpdateObjectはACLを置き換えます。同じ要求で`acl`も更新する場合は拒否します。ComposeObjectとRewriteObjectは公開時にpredefined ACLを適用し、匿名の呼び出し元からの指定は拒否します。rewriteのtokenは`destinationPredefinedAcl`も束縛します。ComposeObjectは、predefined ACLを指定したときだけ`acl`と`owner`を返します。実機記録のCloud Storageと同じ動きです。UpdateBucketのpredefined ACL、`storage_class`、KMS鍵、宛先の`retention`は引き続き拒否します。

同じ2026-11-17に、ComposeObjectとRewriteObjectのリクエストのchecksumを、新しい実機記録（ケース`grpc-gaps-derived-checksums`、[実機記録の証跡（英語）](../en/evidence/provider-differential-gap-probes.md#sixth-recording-storage-v2-request-checksums-on-derived-writes)）に合わせ、誤ったchecksumの記録済みローカル選択を2件外しました。ComposeObjectは、指定されたCRC32Cを、ソースから合成したCRC32Cとバイトを読む前に照合します。合成オブジェクトにはMD5がないため、MD5の指定は値によらずINVALID_ARGUMENT`Invalid argument.`です。RewriteObjectは、16バイトでないMD5を`Provided value (<base64>) is not a base64-encoded 128-bit MD5 hash.`で拒否してから、指定されたCRC32CとMD5をセッション開始前にソースの保存済みハッシュと照合します。tokenはこれらを`destinationPredefinedAcl`の後に束縛するため、別のchecksumを指定した継続はtokenの不一致として拒否します。不一致はCRC32Cを先に調べ、Cloud StorageのCRC32CまたはMD5のメッセージと、それを繰り返す`BadRequest`を付けたINVALID_ARGUMENTを返し、何も書き込みません。合成オブジェクトのように、MD5を示さないソースのrewriteでは、composeと同じくMD5の指定を`Invalid argument.`で拒否します（推定）。JSONのrewriteは指定された`md5Hash`を無視しますが、Storage v2のrewriteはCloud Storageと同じく検証します。ソースに記録されたハッシュとコピーしたバイトが合わない場合は、保存データの破損（UNAVAILABLE）として報告し、クライアントが指定したchecksumの誤りとは扱いません。`grpc_derived_writes_verify_request_checksums`、`grpc_rewrite_tokens_bind_declared_checksums`、`rewrite_tokens_bind_the_stored_parameter_spelling`がこの動作を固定しています。なお、このケースで完了したrewriteの応答には、ソースの`contentType`がありませんでした。gcs-mimicはこれをコピーします。Storage v2がいつ`contentType`を落とすかは未解決です。

2026-11-18に、UpdateObjectが上記のetagを適用するようにしました。`src/grpc/v2/update_object/object_etag.rs`の3つのテストは変更より先に書き、etagを無視していたあいだは失敗しました（古いetagでも更新され、誤った顧客指定の暗号鍵がetagより先に報告されました）。変更後は成功します。異なるetag・引用符付きのetag・不正なetag・前の更新で古くなったetagはオブジェクトを変えずに拒否し、現在のetagは更新を適用し、空のetagは条件になりません。古い世代を選んだ更新はその世代のetagと比べます。存在しないオブジェクトはNOT_FOUNDで、etagの不一致は誤った顧客指定の暗号鍵より先に報告します。テストはetagをGetObjectで読み取ります。`cargo test --locked --offline --lib grpc::`の270件が成功しました。この変更ではSDKのテストも実機への要求も実行していません。

同じ2026-11-18に、テストのなかったUpdateObject・ComposeObject・RewriteObject・RestoreObjectの拒否をテストで固定しました。既存の応答を固定するテストなので、変更なしで成功しました。どの拒否も保存済みのオブジェクトを変えません。

- `update_object_refuses_malformed_and_unmodeled_requests_without_changing_the_object`: objectやupdate_maskの欠落、負のgeneration、同じ属性へのmatchとnot-matchの同時指定、modeまたは`retain_until_time`のないretentionはINVALID_ARGUMENTです。`*`・`contexts`・`contexts.custom`・`storage_class`のmaskはErrorInfo付きのUNIMPLEMENTEDです。
- `compose_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination`: 宛先の欠落、負のソース世代・ソースの条件はINVALID_ARGUMENTです。宛先のretention・KMS鍵・明示的なACLはUNIMPLEMENTEDです。
- `rewrite_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination`: 負のソース世代、負または1 MiBの倍数でない転送量、bucketやnameを含む宛先リソース、不正なtoken、ソースのmetagenerationへのmatchとnot-matchの同時指定はINVALID_ARGUMENTです。宛先リソースのKMS鍵はUNIMPLEMENTEDです。
- `restore_object_refuses_a_missing_generation_and_copying_the_source_acl`: generationの欠落や負の値はINVALID_ARGUMENTです。soft deleteしたソースに対する`copy_source_acl`のtrueと`restore_token`はUNIMPLEMENTEDで、何も復元しません。`copy_source_acl`のfalseは復元します。

matchとnot-matchの同時指定の拒否は、判断が必要な前提条件の検証規則に含まれます。`*`のmask、宛先のretentionとACL、`copy_source_acl`は、下の台帳のケースのとおり実装が必要なものとして残ります。

## 台帳のケース

2026-11-18 に、台帳の各行を、確かめられる振る舞いごとの受け入れケースに分けました。実機の Cloud Storage では実行しないため、Google が定めておらず記録もない応答はローカルな選択とし、固定するテストと合わせて scoped difference に記録します。概要は次のとおりです。

- 意図した拒否（SPEC-GRPC-UNMODELED-SETTINGS の scoped difference）: JSON API も保存しない設定（`rpo`・`website`・`logging`・`encryption`・`billing`・`custom_placement_config`・`hierarchical_namespace`・`ip_filter`・`satisfies_pzs`）への UpdateBucket の mask（それぞれ ErrorInfo まで固定しています）、UpdateObject の`contexts`と`contexts.custom`の mask、ComposeObject の`kms_key`と RewriteObject の`destination_kms_key`、GetIamPolicy・SetIamPolicy・TestIamPermissions での managed folder の IAM リソース、階層型名前空間のバケットだけが使う RestoreObject の`restore_token`。
- 記録済みのローカルな選択: GetIamPolicy・SetIamPolicy のオブジェクトリソースへの UNIMPLEMENTED（proto はバケットと managed folder だけを挙げています）、古いポリシーの etag と、発行する etag の 8 バイトに合わない不正な etag への FAILED_PRECONDITION、小数を含む soft delete の期間の拒否、UpdateBucket の暗黙の lifecycle 確認が 3 回競合した後の ABORTED、UpdateObject の`object.etag`の不一致への FAILED_PRECONDITION と、更新する世代の etag との比較、proto が定めておらず JSON のメタデータ更新では無視される`storage_class`の update mask を UpdateObject が拒否すること（いずれも SPEC-GRPC-LOCAL-CHOICES）。ポリシーのないバケットのロックと 2 回目のロックへの INVALID_ARGUMENT（SPEC-RETENTION-POLICY）。RestoreObject の顧客指定の暗号鍵の拒否（SPEC-GRPC-CSEK）と、owner・ACL を付けた応答（SPEC-GRPC-DERIVED-OWNER-VIEW）。
- 実装が必要なもの: proto が更新の条件とする`bucket.etag`を無視していること、`*`の update mask、Cloud Storage と JSON の PATCH が適用する UpdateBucket の predefined ACL、ポリシーの一部のフィールドだけを指定する SetIamPolicy の mask、proto が定め Cloud Storage が受け付けるオブジェクトへの TestIamPermissions、ComposeObject の宛先の`storage_class`、ComposeObject・RewriteObject の宛先の`retention`・`acl`・出力専用フィールド、RestoreObject の`copy_source_acl`。
- テストが必要なもの: strict の LockBucketRetentionPolicy、proto の切り捨てに従う`retention_policy.retention_duration`の秒単位への切り捨て（CreateBucket は同じ入力をまだ拒否します）、PAP と ACL の変更で別に確かめる`storage.buckets.setIamPolicy`、条件付きバインディングと監査設定の拒否、ComposeObject のソースの`storage.objects.get`と上書き時の`storage.objects.delete`、ACL 読み取り権限のない呼び出し元への predefined ACL 付き ComposeObject の応答、もう一方の転送方式で使う rewrite token。
- 判断が必要なもの: RPC をまたいだバケット・オブジェクトの前提条件の検証規則（UpdateObject・MoveObject・RestoreObject・RewriteObject・ComposeObject は metageneration 条件の 0 と match・not-match の同時指定を拒否し、オブジェクトの読み取り・DeleteObject・書き込みはそのまま評価します。規則で応答が変わるか確定する行には、それぞれこのケースを記録しています。LockBucketRetentionPolicy が 0 を拒否するのは、proto が正の値を必須としているためです）、MoveObject の権限（JSON の move と同じく`storage.objects.get`・`.delete`・`.create`を確かめますが、proto は`storage.objects.move`・`.create`と、上書き時だけの`.delete`を挙げています）、rewrite したオブジェクトにソースの content type を残すか（記録した Cloud Storage の完了応答にはありませんでした）。

GetIamPolicy（RPC-STORAGE-006）自体には未完了のケースがありません。ただし Storage v2 のほかの行と同じく BEHAVIOR-RPC-001 に依存します。どの RPC も、そのルーティングとリクエストのメタデータの処理を通るためです。このため、BEHAVIOR-RPC-001 のルーティングを確かめる SDK、サーバー側の`grpc-timeout`、gzip のメッセージ圧縮が未完了のあいだは PARTIAL のままです。
