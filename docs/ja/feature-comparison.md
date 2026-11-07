# エミュレーターの機能比較

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

同じ行では、すべての製品を同じ操作・範囲で評価します。
対応予定の機能は加点しません。✅は記載範囲で利用可能、◐は一部不足または未検証、
❌は利用不可です。API全体との完全互換を表す記号ではありません。

[English](../en/feature-comparison.md) ·
[実装状態の原本](../../compatibility/features.json)

比較対象は、gcs-mimic（gcs-mimicの列は`0.5.0`に更新）、fake-gcs-server`v1.56.1`、
gcp-storage-emulator`v2026.07.19`です。基本比較は2026-10-23、
ファイル操作の追加調査は2026-10-24、同期ディレクトリの追加調査は2026-10-28時点です。

## バケットとファイル操作

| 同じ範囲で比較する操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| バケットの作成・取得・空バケット削除 | ✅ | ✅ | ✅ |
| バケット一覧のprefix・ページ分割 | ✅ | ✅ | ◐ 全件一覧のみ |
| ファイルの保存・全体取得・削除 | ✅ | ✅ | ✅ |
| JSON multipartアップロード | ✅ | ✅ | ✅ |
| 再開可能アップロードの開始・送信・確定 | ✅ | ✅ | ✅ |
| 単一Rangeの取得 | ✅ | ✅ | ◐ 開始位置が必要 |
| オブジェクト一覧のページ分割 | ✅ | ✅ | ❌ |
| 現在のファイル本体をコピー | ✅ | ✅ | ✅ |
| 1回で完了するrewrite | ✅ | ✅ | ✅ |
| 継続トークンを使うrewrite | ✅ | ❌ | ❌ |
| 複数オブジェクトのcompose | ✅ | ✅ | ✅ |
| オブジェクトの世代履歴 | ✅ 永続化 | ◐ メモリのみ | ❌ |
| オブジェクトのsoft delete・復元（バケット・一括は除く） | ✅ | ❌ | ✅ |

コピーとrewriteはファイル本体の比較で、ACLや所有者の完全な再現を含みません。
Rangeは開始・終了・末尾からの取得が対象で、If-Rangeや透過的な展開は除きます。
composeはgcs-mimicの宣言済みローカル範囲で、1〜32個の世代を固定した入力を扱います。
soft delete・復元は[SDKスコアカード](../../sdk-scorecard/results/2026-10-29-5/summary.md)の
5つのSDKでの実行結果に基づきます。gcs-mimicのsoft deleteは既定で無効で、1秒以上の任意の保持期間を受け付けます。どちらもCloud Storageとは異なります。

| メタデータの操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| 応答の`fields`選択 | ✅ 対応リソース | ❌ | ❌ |
| JSON読み取りのETag条件 | ✅ 対象GET | ❌ | ❌ |
| バケットlabels・既定classのPATCH | ✅ | ❌ 指定を無視 | ◐ キー単位の削除なし |
| バケットlabels・既定classのPUT | ✅ | ❌ | ❌ |
| オブジェクトの標準・独自メタデータPATCH | ✅ | ◐ null削除なし | ◐ マップを置換 |
| オブジェクトの標準・独自メタデータPUT | ✅ | ◐ 一部項目を保持 | ❌ |

バケットのPATCHは省略項目を保持します。PUTは指定したlabelsのマップを全置換します。
ネイティブのプロファイルでは、省略したlabels・versioning・CORS・lifecycle・保持ポリシー（Cloud Storageでの記録どおり）と
既定のイベントホールド（記録した他の項目から推定）を消します（storageClass・soft delete・アクセス設定は保持）。
レガシー互換のプロファイルは省略項目を保持します。
オブジェクトのPUTは省略項目のリセットを含む置換で比較します。
標準メタデータはContent-Typeなどの内容属性で、オブジェクト本体は変更しません。
対象フィールド、nullの扱い、gcs-mimic独自の既定値は
[英語版の更新条件](../en/feature-comparison.md#metadata-update-comparison-criteria)
を参照してください。Google実環境との未検証事項は残っています。

## URL・権限・ブラウザー

| 同じ範囲で比較する操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| path-styleのGET・HEAD・PUTで本体転送 | ✅ | ✅ public-host経由 | ◐ HEADなし |
| JSON APIの既定CORS | ✅ | ✅ | ❌ |
| バケットごとのXML CORSを強制 | ✅ | ❌ | ❌ |
| 無条件のバケットIAMを保存・取得 | ✅ | ❌ | ✅ 保存のみ |
| 既定ロールのバケットIAMを強制 | ✅ strict | ❌ | ❌ |
| `allUsers`と`allAuthenticatedUsers`の読取権限を区別 | ✅ strict | ❌ | ❌ |
| V4 RSA署名形式のGET・PUTで本体転送 | ✅ | ✅ 検証なし | ✅ 検証なし |
| V4 RSA署名と期限を検証 | ✅ | ❌ | ❌ |
| TLSで待ち受ける | ✅ | ✅ | ❌ |
| gRPC Storage v2を公式Goクライアントで使う（平文h2c。バケットCRUD・更新・IAM、オブジェクトの読み書き・一覧・更新・削除・復元、resumableの開始・照会・取消、範囲・双方向読み取り、rewrite・compose・move、Writer.Append・takeover・確定によるappendableオブジェクト。ゾーンのRapidバケットは扱わない） | ✅ JSON・XMLと状態・strict認可を共有 | ❌ Storage v1のみ | ❌ |
| 旧Storage v1 gRPC（比較対象の12 RPC。複数メッセージのInsertObject、プロジェクト省略のListBucketsを含む） | ✅ 記載したフィールド範囲 | ◐ InsertObjectは最初のメッセージのデータだけ | ❌ |
| ACL操作 | 🚧 バケット・既定オブジェクト・オブジェクトACLのCRUD、PATCH・PUTでの置換、既定ACLの継承。strictではオブジェクトACLをIAMと合わせて判定（[ACLの効果（英語）](../en/slices/acl-permission-effects.md)） | 🚧 メタデータ操作のみ。強制なし | 🚧 メタデータ・一覧のみ。強制なし |
| プロジェクトのHMACキー | 🚧 キーの管理と1回だけ表示するsecret | ❌ | ❌ |
| バケットの通知設定 | 🚧 保存・一覧と、ローカルPub/Subエミュレーターへの配信 | ✅ SDKスコアカードの往復 | ❌ |
| V4署名POST policyのフォームアップロード | ✅ RSA署名・条件・期限を検証 | ◐ RubyはPASS、Goは失敗 | ❌ |
| object retentionと顧客指定キー | 🚧 retentionを削除・上書きで強制。CSEKは読取時に照合し、ディスク上は暗号化しない | ❌ | ❌ |

署名らしいURLを受け付けることと、署名・期限・権限を検証することを分けています。
IAMの保存やCORSの成功も、アクセス制御を保証しません。
gcs-mimicの公開主体判定はローカルのstrictの範囲です。ACLとUBLAの効果と残る条件は[ACLの効果（英語）](../en/slices/acl-permission-effects.md)に記録しています。

## ローカルでの使い方

| 同じ範囲で比較する操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| APIで保存した本体を再起動後も保持 | ✅ | ✅ ファイル方式 | ✅ ディスク方式 |
| APIの保存先をバケット名・ファイル名で見る | ✅ `--sync-dir`で書き出し | ✅ ファイル方式 | ✅ ディスク方式 |
| 外部編集後にhash・世代も整合させる | 🚧 作成・編集を継続監視で取り込み、削除は承認後だけ反映（Linux・macOS） | ❌ | ❌ |
| 任意のメモリ保存モード | ✅ 上限付きRAM（`--backend memory`） | ✅ | ✅ |
| バケット・ファイルの初期ディレクトリ読込 | ✅ 毎回の起動時・明示的に有効にしたreseed | ✅ 起動・reseed | ❌ |
| 公開済みのDockerイメージ | ✅ GHCR・Docker Hub（amd64・arm64） | ✅ | ✅ |
| 公開済みの単体バイナリ | ✅ macOS・Windows・Linux | ✅ | ❌ Pythonが必要 |

fake-gcs-serverは実ファイルの直接編集がGETに反映される場合もありますが、
hashや世代が古いまま残ります。[保存方式の調査](storage-backends.md)を参照してください。
gcs-mimicは起動のたびにreadinessより前に初期ファイルを反映し、
イメージは追加の設定なしで`/data`を読み込みます。fake-gcs-serverと同じく、本体やメタデータが
初期ファイルと異なるオブジェクト（APIでのメタデータ変更を含む）は新しい現行世代になります。
初期ファイルから書き込む内容と完全に同じオブジェクトだけは世代を変えずに残しますが、
fake-gcs-serverはこの場合も新しい世代を書き込みます。fake-gcs-serverは`/_internal/reseed`も備えています。
gcs-mimicは初期ファイルのディレクトリを監視しないため、編集の反映には再起動するか、
0.8.0以降は管理APIを明示的に有効にして`/_internal/reseed`を呼びます。
どちらも、初期ファイルを消したオブジェクトは削除しません。

gcs-mimicの同期ディレクトリは、カタログとは別の写しで、2つ目の保存方式ではありません。
APIの変更は応答の少し後に現れ、外部の作成・編集はhashを計算し直した新しい世代になります。
0.8.0以降はディレクトリを継続して監視し、外部削除はrevisionを固定した承認の後だけ反映し、
競合は[同期CLI](sync-cli.md)で一覧・解決できます。0.7.0以前は起動時だけ取り込み、外部削除と競合の解決はありません。
[使い方と制約](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)を参照してください。
ディスクへの保存は、停電耐性や同等の性能を保証しません。
速度・メモリ・サイズは、一般には製品間で順位を付けません。オブジェクトの読み取りに限ったfake-gcs-serverとの同条件の比較が1件あり、[読み取り経路の計測（英語）](../en/evidence/read-path-performance.md)に記録しています。

## 検証の範囲

他製品は固定した版のソースと文書を調査しています。
fake-gcs-serverにはバケットAPI比較とmacOSのファイル・CORS確認を実行しましたが、
製品全体のテストや他製品でのブラウザー検証は実施していません。
gcs-mimicのブラウザー検証は、信頼したHTTPSを使うChromiumの宣言済み範囲です。

操作ごとの証跡・出典と省略した詳細は[英語の比較表](../en/feature-comparison.md)、
機能IDごとの状態は[生成された機能表](feature-matrix.md)を参照してください。
このページは台帳と別の実装状態を管理しません。
