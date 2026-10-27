# UBLA と predefined ACL の扱い

対象は DESIGN-019、BEHAVIOR-BUCKET-013、JSON-BUCKETS-005 の
iamConfiguration 入力範囲です。状態の正本は機能台帳で、広い機能行は PARTIAL を維持します。
[英語の契約と検証結果](../en/slices/sdk-ubla-predefined-acl.md)を参照してください。

バケット作成時に `iamConfiguration.uniformBucketLevelAccess.enabled=true` を
指定できます。GET・一覧・再起動後も設定と、有効化から90日後の `lockedTime` が返ります。
旧名の `bucketPolicyOnly` も同じ値を読み書きできます。
両方の `enabled` が矛盾すると現行名の値を優先します。Rubyの通常の更新が、
GET応答の旧名を持ち越すためです。直接送る場合は現行名だけを指定してください。
無指定・空・nullのUBLA設定は指定なしと同じです。無効時は両方に
`enabled:false` を返し、`lockedTime` は返しません。

PATCH・PUTで有効化と無効化ができ、メタ世代と `storage.buckets.update` の
権限を検証します。無指定のUBLAはどちらの更新でも保存済みの値を維持します。
90日以上連続して有効なバケットは無効化できません。
期限前に無効化してから再有効化すると、新しい90日の期限になります。
PAPの `enforced` とUBLAを同時に指定して作成することもできます。

UBLAが有効な場合、バケット作成・PATCH・PUTの `predefinedAcl` と
`predefinedDefaultObjectAcl`、オブジェクト作成・copy・rewrite・composeの
`predefinedAcl` または `destinationPredefinedAcl` は400で拒否し、
該当パラメーターを外してIAMを使うよう案内します。
UBLAが有効な場合のreasonは `invalidArgument` です。
無効な場合は共通の未対応応答である400 `badRequest` になります。
ACLを適用したふりはしません。

カタログはschema 22から23へ移行し、既存バケットはUBLA無効のままです。
現在の認可はIAMのみなので、UBLAの切り替えによって認可結果は変わりません。
ACLのCRUD、保存済みACLの復元、作成時の追加IAM既定権限、
初期化ファイルのUBLA指定、ACL・UBLA・PAPの残る相互作用は未対応です。
null・空値・別名の矛盾やエラー詳細について、Google実環境との完全一致は未検証で、
SPEC-UBLA-EDGESに記録しています。

Go・Rubyの追加テストで修正前の失敗を確認し、Rustの対象テストと型検査は通過しました。
HTTPの `just check-sdk` はGo全件とRubyの通常・strict・移行の行程が通過しました。
`just check` も全件成功しました。型・lint・クロスコンパイル・Rustの全テスト・
生成台帳・シェルの検査を含みます。regional・fail-fast対応との統合後も両SDKの行程が通っています。
最終検証日は2026-10-27で、結果は英語の契約文書の検証節にまとめています。
指定に従いHTTPSのSDK、コンテナ、ガイドの通し、実クラウド、ベンチマークは実行しません。
