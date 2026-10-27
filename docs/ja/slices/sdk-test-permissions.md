# バケットの権限確認

対象はJSON-BUCKETS-012とDESIGN-028です。両行はPARTIALのまま、再現精度はLOCAL_SIMULATIONです。
詳細な契約と検証結果は[英語の正本](../../en/slices/sdk-test-permissions.md)を参照してください。

## 対応する動作

GET `/storage/v1/b/{bucket}/iam/testPermissions`に、確認する権限を`permissions`で繰り返し指定します。
応答には`kind: storage#testIamPermissionsResponse`と、呼び出し元が持つ権限の配列が入ります。
このメソッドの呼び出し自体には権限を要求しません。strictでは既存の認証検証を維持します。

strictでは実操作と同じ認可関数を使い、プロジェクトとバケットの権限を評価します。
permissiveでは認可しない既存の意味に合わせ、対応する要求権限をすべて返します。
重複する権限は一度だけ返します。複数権限の確認を一つのトランザクションにはまとめません。

権限の指定なし・不正な名前・プロジェクトだけに適用するバケット作成と一覧の権限は400です。
存在しないバケットは404です。これらのエラーの詳細はローカルの選択としてSPEC-TEST-PERMISSIONSに残しています。
既知の未対応権限と空でない`userProject`は400 `badRequest`です。空の`userProject`は省略と同じです。

## 受け入れ条件と制限

TEST-PERMISSIONS-ROUTEとTEST-PERMISSIONS-STRICT-EFFECTIVEを対象とします。
TEST-PERMISSIONS-PERMISSIVEとTEST-PERMISSIONS-SDKも対象です。
通信テストで入力と応答、許可と拒否の混在、権限の付与と取り消しを確認しました。
GoとRubyの既定の高水準APIは、strictとpermissiveの両方で通りました。

オブジェクトの権限確認、他の権限、Requester Pays、ACL・UBLA・PAPの相互作用の拡張、
実サービスとのエラーの一致は未対応です。台帳ではPENDINGとして残します。
コンテナとガイド全体の検証は対象外です。実クラウドでの比較と性能測定は行っていません。
