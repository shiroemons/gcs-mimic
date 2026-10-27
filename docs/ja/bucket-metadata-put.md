# バケットPUTで省略した設定を保持する

JSON-BUCKETS-013はPARTIAL、再現精度はLOCAL_SIMULATIONのままです。
Ruby google-cloud-storage 1.62.1の`bucket.update`ブロックは、変更した属性だけをPUTで送ります。
PAPだけを変更しても、既存のversioningとlabelsが残るようにしました。
属性への直接代入は引き続きPATCHを使います。新しい設定は不要です。

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
RubyとGoの回帰、全設定の省略維持と再起動、明示null・空設定をRustで確認しています。
受け入れケースと検証結果は[英語の契約文書](../en/slices/bucket-metadata-put.md)を参照してください。

2026-10-27にネイティブのPUTガイドのブロックを1回実行し、1秒未満で成功しました。
指定した設定の置換、省略した設定の維持、古い条件での412、失敗後のGET一致を確認しています。
PUT-GUIDEのPASSはこのネイティブのブロックが対象です。Dockerと再起動は再検証していません。
PUT-ASSERTを新しい契約と未対応エラーに合わせて変更した後、比較は再実行していないため、
PUT-REFERENCEはPENDINGに戻しています。
