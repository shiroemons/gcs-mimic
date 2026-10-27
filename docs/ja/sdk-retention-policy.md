# バケットの保持ポリシー

対象はJSON-BUCKETS-007、BEHAVIOR-OBJECT-036と、バケット作成・取得・一覧・更新の保持設定です。
仕様未確定のエラー詳細と未対応の相互作用があるため、台帳の状態はPARTIALです。
[英語の契約と検証結果](../en/slices/sdk-retention-policy.md)を正本とします。

JSON APIのinsert・PATCH・PUTで`retentionPolicy.retentionPeriod`を設定できます。
期間は1秒以上3,155,760,000秒未満です。`effectiveTime`と`isLocked`を返します。
ロックされていなければ期間を変更でき、PATCHの`retentionPolicy:null`で削除できます。
PUTで保持設定を省略した場合も削除します。オブジェクトには作成時刻と現在の期間から
計算した`retentionExpirationTime`を返します。

保持期間中の削除と同名上書き、copy・rewrite・composeの宛先上書き、move元の削除、
composeの元オブジェクト削除は`403 retentionPolicyNotMet`になります。
XML PUTも共有判定を通ります。一時ホールドとevent-based holdも独立して削除・置換を拒否します。
wireテストでは注入クロックを進め、待機せずに期限到達時の成功を確認します。

`POST /storage/v1/b/BUCKET/lockRetentionPolicy?ifMetagenerationMatch=N`でロックします。
最新のmetagenerationが必要です。ロック後は削除と短縮を拒否し、延長を許可します。
削除・短縮は原因と延長例付きの`400 badRequestException`、IAM拒否は`403 forbidden`です。
strictモードでは設定とロックの両方に`storage.buckets.update`が必要です。
ロックはエミュレーター内の空バケットでだけ試験します。

Goは`BucketAttrs.RetentionPolicy`と`BucketHandle.LockRetentionPolicy`、Rubyは
`bucket.retention_period=`と`bucket.lock_retention_policy!`を使えます。
追加の設定キーと依存はありません。スキーマ25への列追加で設定を保存します。

受け入れケースはRETENTION-CONFIG、RANGE、METADATA、PROTECT、CLOCK、LOCK、HOLDS、
AUTH、PERSIST、SDKです。初期化ファイル、XMLの保持ポリシーAPI、object retention、
ライフサイクル・soft deleteとの相互作用、event-based hold解除時の保持期間の再開始はPENDINGです。
`null`・空設定の正規化と一部のエラー詳細は、SPEC-RETENTION-POLICYにローカルの選択として記録しています。
現実装はevent-based hold解除時に保持の起点を取り直さず、作成時刻から期限を計算し続けます。

Go・Rubyの全SDKテスト、保持wireの4件とスキーマ移行テストは通過しています。
amd64・arm64のコンテナー通し検証と依存の最新版確認も通過しています。
最終の`RUST_TEST_THREADS=8 just check`は1,034件成功、失敗・無効化0件です。
最終の全体検証結果は[英語文書の検証節](../en/slices/sdk-retention-policy.md#verification)を参照してください。
