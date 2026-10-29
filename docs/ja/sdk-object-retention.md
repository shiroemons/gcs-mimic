# オブジェクトの保持設定（Object Retention Lock）

対象はBEHAVIOR-OBJECT-034と、JSON APIのバケット作成の`enableObjectRetention`、
オブジェクトのアップロード・取得・一覧・更新の`retention`です。
未対応のケースが残るため、台帳の状態はPARTIALです。
[英語の契約と検証結果](../en/slices/sdk-object-retention.md)を正本とします。

`POST /storage/v1/b?enableObjectRetention=true`で作成したバケットは
`objectRetention: {"mode":"Enabled"}`を返します。作成後の有効化と無効化はできません。
リクエスト本文の`objectRetention`は、保存済みの状態と一致する場合だけ受け付けます。

multipart・resumableアップロードとPATCH・PUTで`retention {mode, retainUntilTime}`を設定でき、
取得・一覧・アップロード応答で読み戻せます。`mode`は`Unlocked`か`Locked`です。
`null`・`{}`・全項目nullは設定の削除で、PUTで省略した場合は保持します。
Unlockedは延長が自由で、ロック・短縮・削除には`overrideUnlockedRetention=true`が必要です。
Lockedは同じかより後の時刻への延長だけを受け付け、期限後も削除できません。

`retainUntilTime`より前の削除（世代指定とsoft deleteを含む）と同名アップロードによる置換は
`403 retentionPolicyNotMet`です。バージョニング有効のバケットでは、保持中のライブ世代を
非現行にできますが、非現行世代の削除は拒否します。期限は注入クロックで判定します。
機能が無効なバケット、event-based hold中のオブジェクト、100年を超える期限は
`400 invalidArgument`です。copy・rewrite・composeの宛先`retention`は`400 badRequest`で拒否し、
コピー元の設定も引き継ぎません。ホールドとバケットの保持ポリシーとは独立に判定し、
`retentionExpirationTime`は両者のうち遅い期限を返します。

strictモードでは、作成時の有効化に`storage.buckets.enableObjectRetention`、`retention`を含む
アップロード・更新に`storage.objects.setRetention`、`overrideUnlockedRetention=true`に
`storage.objects.overrideUnlockedRetention`が必要です。スキーマ29への列追加で保存し、
既存のバケットは無効、既存のオブジェクトは設定なしとして移行します。

gRPCは`Bucket.object_retention`と`Object.retention`を返しますが、設定の入力は明示的な
未対応エラーです。copy・rewrite・composeでの設定、XMLのヘッダー（EXT-XML-016/027）、
ライフサイクルとの相互作用、初期化ファイル、プロジェクトのlien、プロバイダーと完全に
一致するエラー詳細はPENDINGです。ローカルの選択はSPEC-OBJECT-RETENTIONに記録しています。
