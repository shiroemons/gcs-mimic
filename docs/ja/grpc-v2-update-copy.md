# Storage v2の更新・コピー

<!-- markdownlint-configure-file {"MD013": false} -->

対象はRPC-STORAGE-005〜010、012、017、021、024とBEHAVIOR-RPC-002です。正本は[英語の契約](../en/slices/grpc-v2-update-copy.md)です。全行をPARTIALとし、未対応範囲を台帳に残します。

UpdateBucket・UpdateObject・TestIamPermissionsの再現精度は、共有するJSONの行と同じLOCAL_SIMULATIONです。ほかの実装済み範囲はEXACTですが、未対応のケースを残したPARTIALであり、API全体の一致は主張しません。

バケット・オブジェクトのfield mask更新、バケットIAMの取得・保存・権限検査、保持ポリシーのロック、rewrite・compose・同一バケット内の原子的なmove・soft delete世代のrestoreを、JSONと同じカタログ・認可・検証で処理します。JSONで用意したデータへのgRPC更新を、JSONで確認できます。本文上限は既存設定を使い、共有処理が64 KiBのバッファで転送します。新しい設定や依存は追加しません。

対応するmaskは、labels・versioning・CORS・lifecycle・保持・soft delete・UBLAなど、JSONで対応済みの項目に限ります。バケットでは`acl`・`default_object_acl`も更新できます。オブジェクトではmetadata・固定メタデータ・custom time・holdに加え、`acl`（オブジェクトACLの置換）と`retention`（object retentionの設定・削除）を更新できます（[SDK互換（英語）](../en/slices/grpc-v2-sdk-parity.md)）。指定しない項目は保持し、未対応maskはErrorInfo付きのUNIMPLEMENTEDです。Go SDKが空の更新で送る出力専用のcreate_timeは無視します。metadataとlabelsのマップ全体は、共有の更新トランザクション内で置き換えます。暗黙の前提条件は追加せず、クライアントが明示した条件だけを適用します。キー単位の更新は指定したキーだけをマージします。

iam_configの親maskでは、存在するUBLA・PAPだけを更新します。UBLAだけを変更してもPAPは保持し、PAPだけを変更してもUBLAは保持します。versioning・保持・soft delete・lifecycleなどの親maskも確認し、単一の編集項目やリストの明示的な解除・置換は維持します。

UpdateBucketはprotoから共有の`BucketMetadataUpdate`を直接作り、JSON本文の組み立てと再解析を行いません。
UpdateObjectも`ObjectMetadataUpdate`を直接作り、ACL・保持時刻を型付きで渡します。
WriteObject・ComposeObject・RewriteObjectはprotoのメタデータ変換と共有検証を再利用します。
rewrite tokenの照合用保存形式は既存互換を保ちますが、適用時のJSONパーサーは通りません。
SetIamPolicyは共有の型付きポリシーを直接作ります。TestIamPermissionsは渡された権限文字列をそのまま検証し、クエリ文字列を組み立てません。
権限の重複は入力順に除き、入力サイズはgRPCメッセージ上限で制限します。権限ごとのカタログ呼び出しは残ります。
空の`iam_config`親maskも更新としてmetagenerationを進めますが、設定自体は保持します。
この空更新の扱いはGoogleで未観測のため、SPEC-GRPC-EMPTY-IAM-UPDATEに記録しています。

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
