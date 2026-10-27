# オブジェクトのsoft deleteと復元

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

対象はBEHAVIOR-BUCKET-015、BEHAVIOR-OBJECT-030、BEHAVIOR-OBJECT-031、JSON-OBJECTS-011です。
未対応の相互作用と仕様差が残るため、4行ともPARTIALです。
[英語の契約と検証結果](../en/slices/sdk-soft-delete-restore.md)を正本とします。

バケットのinsert・PATCH・PUTで`softDeletePolicy.retentionDurationSeconds`を設定します。
0は無効、604800秒から7776000秒は有効です。範囲外の値は状態を変えず400で拒否します。
get・listで保持秒数と`effectiveTime`を返し、期間を延ばしたときだけ有効時刻を更新します。
空設定と`null`は省略と同じです。GCSの新規バケットの既定7日とは異なり、
ローカルでは省略時に無効とし、予期しないディスク保持と既存利用者の動作変更を避けます。
これらの仕様差と90日境界の資料間の違いはSPEC-SOFT-DELETEに記録しています。

既存の`bucket`に7日を設定する例です。固定したGo Storage v1.70.0と
Ruby Storage 1.62.1のSDKテストで確認しています。
Goでは`storage`・`time`のimportと既存の`ctx`を前提にします。

```go
_, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}})
```

```ruby
bucket.soft_delete_policy = { retention_duration_seconds: 604800 }
```

Goでは削除前に`err`を確認してください。未設定の`softDeleted=true`を空の200にする
動作（公式は400）はオーナー承認済みです。`--log-level debug`では、ポリシー未設定の
空一覧に理由と`retentionDurationSeconds=604800`の修正例をリクエストごとに1行出します。
応答は変えていません。[今回の検証](../en/slices/sdk-local-auth-softdelete-notes.md)を参照してください。

有効なバケットではJSON・XMLの削除と、バージョニング無効時のアップロード・copy・rewrite・
compose・move宛先上書きで、古い世代の本文を残します。バージョニング有効時の宛先上書きは
noncurrentにし、その世代を明示して削除するとsoft-deletedになります。
ホールドと保持ポリシーは先に判定します。削除時点の期間から決まった`hardDeleteTime`は、
後からバケットの期間を変えても変わりません。

通常の取得・一覧から削除済み世代を隠し、`softDeleted=true`でメタデータを取得できます。
GETには`generation`が必要で、`alt=media`は使えません。一覧は既存の絞り込みとページングを使い、
`versions=true`との併用を拒否します。
ポリシーの省略と0は同じ扱いです。`softDeleted=true`の一覧は200で、保持中の世代がなければ
空になります。該当世代がなければメタデータGETとrestoreは404を返します。
公式の一覧・restoreはポリシーなしの400を定めており、この扱いはローカルの仕様差です。

`POST /storage/v1/b/BUCKET/o/OBJECT/restore?generation=N`は新しいlive世代を作り、
metagenerationを1に戻します。元の削除済み世代は期限まで残ります。同名のlive世代と前提条件を
同じトランザクションで判定し、上書きが必要な場合は削除権限も確認します。
strictでは`storage.objects.restore`と`storage.objects.create`が必要です。
`copySourceAcl=false`は受け付け、true・`restoreToken`・full projectionは未対応として400で拒否します。
Rubyが送るform本文は、検証済みURL queryと完全に一致する場合だけ受け付けます。

注入クロックで期限切れを判定し、取得・一覧・復元で存在しない扱いにします。
同じバケットの削除・削除済み一覧・復元のついでに、最大100件ずつ回収します。
タイマー、ポーリング、起動時の全件走査は追加していません。
soft-deletedだけが残るバケットは完全削除でき、その世代も削除します。
バケット自体のsoft deleteとJSON-BUCKETS-010はNOT_STARTED、初期化ファイルの設定はPENDINGです。
HNS、ACLコピー、Autoclass、ライフサイクル実行と伝播の再現もPENDINGです。

スキーマ26への列追加で設定と削除済み世代を永続化します。新しい設定キーや依存はありません。
対象Rustテスト7件、Go・RubyのHTTP全SDK検証は通過しています。
最終の`RUST_TEST_THREADS=8 just check`も通過し、591行の台帳と生成マトリクスを検査しています。
受け入れケースはSOFT-DELETE-CONFIG、DEFAULT、DELETE、OVERWRITE、LIST-GET、RESTORE、
EXPIRY、BUCKET-DELETE、AUTH、PERSIST、SDKです。検証の詳細は英語の検証節を参照してください。
HTTPS、コンテナ、ガイドの通し実行、実クラウド照合、性能測定はこのスライスでは実施していません。
