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

対応するmaskは、labels・versioning・CORS・lifecycle・保持・soft delete・UBLAなど、JSONで対応済みの項目に限ります。バケットでは`acl`・`default_object_acl`も更新できます。オブジェクトではmetadata・固定メタデータ・custom time・holdに加え、`acl`（オブジェクトACLの置換）と`retention`（object retentionの設定・削除）を更新できます（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。指定しない項目は保持し、未対応maskはErrorInfo付きのUNIMPLEMENTEDです。Go SDKが空の更新で送る出力専用のcreate_timeは無視します。metadataとlabelsのマップ全体は、共有の更新トランザクション内で置き換えます。暗黙の前提条件は追加せず、クライアントが明示した条件だけを適用します。キー単位の更新は指定したキーだけをマージします。

iam_configの親maskはメッセージ全体を置き換えます。省略したPAPはinheritedに戻り、省略したUBLAは無効になります。Cloud Storageは空の`iam_config`でPAPをinheritedに戻しました（2026-11-13に記録したケース`grpc-empty-iam-update`）。UBLAが有効なバケットで無効になる点は同じ規則からの推定で、記録はありません。Go Storage 1.70.0は個別の設定でも親maskを送るため、片方だけを指定するともう片方も戻ります。`iam_config.public_access_prevention`などの末端maskは、その設定だけを変えます。versioning・保持・soft delete・lifecycleなどの親maskも確認し、単一の編集項目やリストの明示的な解除・置換は維持します。

UpdateBucketはprotoから共有の`BucketMetadataUpdate`を直接作り、JSON本文の組み立てと再解析を行いません。
UpdateObjectも`ObjectMetadataUpdate`を直接作り、ACL・保持時刻を型付きで渡します。
WriteObject・ComposeObject・RewriteObjectはprotoのメタデータ変換と共有検証を再利用します。
rewrite tokenの照合用保存形式は既存互換を保ちますが、適用時のJSONパーサーは通りません。
SetIamPolicyは共有の型付きポリシーを直接作ります。TestIamPermissionsは渡された権限文字列をそのまま検証し、クエリ文字列を組み立てません。
権限の重複は入力順に除き、入力サイズはgRPCメッセージ上限で制限します。権限ごとのカタログ呼び出しは残ります。
空の`iam_config`親maskも更新としてmetagenerationを進め、両方の設定を戻します。
この動作は2026-11-13の記録で確認し、SPEC-GRPC-EMPTY-IAM-UPDATEは解決済みです。

バケットの共有検証で返るAutoclass・lifecycle・保持エラーも、gRPCではprotoのフィールド名で案内します。
更新トランザクション内で検出するエラーも対象です。JSONの文言は維持し、gRPCの保持期間の案内にはJSON本文の例を入れません。

rewriteは共有の継続セッションを使い、token単独での継続と直前tokenの再送に対応します。tokenはgRPC内で使い続けてください。JSONとのtoken共用は対象外です。composeは1〜32個で世代・前提条件を検査します。暗号化・派生オブジェクトのACL・条件付きIAM・明示チェックサム検証・派生オブジェクトのcustom time/temporary holdなどは未対応です。lifecycleは検証・保存の範囲であり、実行処理は追加しません。

同じ種類の世代にmatchとnot-matchを同時に指定するとINVALID_ARGUMENTです。JSONの複合条件の優先順位までは、この範囲では再現しません。

ComposeObject・RewriteObject・MoveObject・RestoreObjectの書き込み結果も共通のowner・ACL表示処理を使います。
ACL読み取り権限がない場合とUBLAでは、owner・ACLを返しません。
これらの派生RPCはWriteObject・UpdateObjectの観測からの推定で、Googleでは未観測です。
SPEC-GRPC-DERIVED-OWNER-VIEWに記録し、保存する完了応答は呼び出し元に依存させず、返却時に現在の権限を適用します。

Go Storage 1.70.0の既定の高水準APIを使い、permissiveとstrictを検証します。型検査後、Rustでmask・世代条件・保持ロック・分割rewriteの再送・32/33個の境界を検証します。コンテナ・ガイド・性能測定・実Google Cloudとの比較は実施しません。最終検証結果は英語版に記録します。

IAM設定の保持については、修正前にRustとGoの両方で失敗を再現しました。最終ソースで担当Rustテスト5件とjust check-sdkが成功し、Goの更新・strict・foundationも確認しています。共通のstatus::from_jsonを使い、410はFAILED_PRECONDITION、501はErrorInfo付きUNIMPLEMENTEDです。未対応呼び出しの確認にはRapidの書き込みを使います。CancelResumableWriteは[オブジェクト入出力](grpc-v2-object-io.md)の契約で実装しています。HTTP実行のため、既存のGo V2署名テスト1件だけスキップしました。

bucket・object I/Oを統合した最終ソースでjust checkも成功しました。ライブラリ528件、全統合テスト、native・Windows・Linuxのclippy、台帳591行と生成表の整合性を確認済みです。IAM設定の保持の問題は解消し、ほかの親maskは単一項目の解除・置換として正しいため変更しません。

2026-11-14に、テストのなかった未対応オプションの拒否を3つのテストで固定しました。どのオプションも、状態を変える前に、オプション名を含むErrorInfo付きUNIMPLEMENTEDを返します。UpdateBucket・UpdateObjectのpredefined ACLはメタデータとmetagenerationを変えません。ComposeObject・RewriteObjectの暗号・ACL・checksum・`storage_class`・`temporary_hold`・`custom_time`と、RestoreObjectの`restore_token`は、宛先オブジェクトを作りません。SetIamPolicyの`bindings`と`etag`以外のmaskと、managed folder・objectのIAMリソースはポリシーを変えません。`cargo test --locked --offline --lib grpc::`の168件が成功しました。RPC-STORAGE-006・007・009・010・012・017・021には成功ケース`grpc_unsupported_option_rejections`として記録し、`grpc_full_contract`は残りのフィールド・エラーの組み合わせ・実機との一致のためPENDINGのままです。

これらのテストが固定しているのはgcs-mimic自身の拒否で、Cloud Storageも同じ要求を断ることを示すものではありません。実機記録（UTC で 2026-11-13、ケース`grpc-unsupported-options`）では、Cloud StorageはpredefinedのACL、UpdateObjectの`*`マスク、派生書き込みの`custom_time`、ポリシーのバージョン2、オブジェクトへのTestIamPermissionsを受け付けました。誤ったchecksumはINVALID_ARGUMENT、uniform accessのないバケットのmanaged folder IAMはFAILED_PRECONDITIONで断りました。gcs-mimicはポリシーのバージョン2を受け付けるように変え、ほかの拒否はSPEC-SDK-UNSUPPORTEDの記録済みローカル選択として残します。また、ComposeObjectとMoveObjectの応答には`acl`と`owner`が含まれず、RewriteObjectには含まれると分かったので、gcs-mimicもこの2つのRPCでは省きます。
