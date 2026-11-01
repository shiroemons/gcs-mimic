# バケットPUT

JSON-BUCKETS-013はPARTIAL、再現精度はLOCAL_SIMULATIONのままです。

## ネイティブのプロファイル（2026-11-01以降）

記録した実際のCloud Storageは、PUTでリソース全体を置き換えていました。
そのためネイティブのプロファイルでは、省略したlabels、CORS、lifecycle、versioning、retentionPolicyを消します。defaultEventBasedHoldも消しますが、記録にこのフィールドはなく、他のフィールドからの外挿です。
ロック済みのretentionPolicyは残します。storageClass、softDeletePolicy、アクセス設定（PAP・UBLA）は省略しても保ちます。
`predefinedAcl`を適用し、本文の`name`がパスと違うときは400を返します。
PUTで更新を送るクライアント（生成されたGo APIの`Buckets.Update`など）では、省略した設定が消えます。
Ruby google-cloud-storage 1.62.1の`bucket.update`ブロックもPUTを送るため、触れなかったversioningとlabelsが消えます。
属性への直接代入はPATCHなので、名前を挙げなかった設定を保ちます（Rubyの受け入れケース`bucket_put_ruby_update_block_replaces_and_setters_patch`）。

## レガシー互換のプロファイル

Ruby google-cloud-storage 1.62.1の`bucket.update`ブロックは、変更した属性だけをPUTで送ります。
PAPだけを変更しても、既存のversioningとlabelsが残るようにしました。
属性への直接代入は引き続きPATCHを使います。新しい設定は不要です。
以下の「省略した設定を保つ」説明が当てはまるのは、このプロファイルだけです
（ネイティブでは、storageClass、softDeletePolicy、アクセス設定に限ります）。
こちらはローカルの互換上の選択のままです。

省略した項目は現在の設定を保ちます。
labels、storageClass、versioning、defaultEventBasedHold、CORSが対象です。
PAP・UBLA、lifecycle、retentionPolicy、softDeletePolicyも同じ扱いです。
空の本文ではなく、空オブジェクト`{}`を送る場合も設定を保ち、metagenerationを1増やします。
指定したlabelsはマップ全体を置き換え、nullまたは空マップで消します。
`versioning:{}`は無効化します。明示したnull・空値の項目別の契約、
不正入力の400、64 KiBを超える本文の413、未対応の意味を持つ値の400 badRequestは維持します。
権限・更新条件・IAMポリシー・既存オブジェクトの保護も変えません。

この方針はRuby SDKの長年の呼び方から選んだローカルの互換動作です。
Googleの資料には、省略したメタデータを変更する場合もあると記載されています。
実GCSでは確認していません。
SPEC-BUCKET-PUTに選択と未検証事項を記録しています。
全設定の省略時の扱いと再起動、明示null・空設定をRustで確認しています。RubyとGoのSDKテストは、ネイティブの置き換えに合わせて直している最中で、再実行するまでこのプロファイルの確認に数えません。
受け入れケースと検証結果は[英語の契約文書](../en/slices/bucket-metadata-put.md)を参照してください。

2026-10-27にネイティブのPUTガイドのブロックを1回実行し、1秒未満で成功しました。2026-11-01には、空PUT後の確認をネイティブの置き換え（labels・versioning・既定ホールドを消し、storageClassは保つ）に合わせて実行し直し、成功しました。
指定した設定の置換、省略した設定の扱い、古い条件での412、失敗後のGET一致を確認しています。
PUT-GUIDEのPASSはこのネイティブのブロックが対象です。Dockerと再起動は再検証していません。
PUT-ASSERTを新しい契約と未対応エラーに合わせて変更した後、比較は再実行していないため、
PUT-REFERENCEはPENDINGに戻しています。
