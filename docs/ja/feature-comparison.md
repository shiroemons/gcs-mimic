# エミュレーターの機能比較

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

同じ行では、すべての製品を同じ操作・範囲で評価します。
対応予定の機能は加点しません。✅は記載範囲で利用可能、◐は一部不足または未検証、
❌は利用不可です。API全体との完全互換を表す記号ではありません。

[English](../en/feature-comparison.md) ·
[実装状態の原本](../../compatibility/features.json)

比較対象は、公開前の開発中のgcs-mimic（初回のイメージは`0.1.1`）、fake-gcs-server`v1.56.1`、
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
| soft delete・復元 | ❌ | ❌ | ◐ 独自の削除・復元 |

コピーとrewriteはファイル本体の比較で、ACLや所有者の完全な再現を含みません。
Rangeは開始・終了・末尾からの取得が対象で、If-Rangeや透過的な展開は除きます。
composeはgcs-mimicの宣言済みローカル範囲で、1〜32個の世代を固定した入力を扱います。

| メタデータの操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| 応答の`fields`選択 | ✅ 対応リソース | ❌ | ❌ |
| JSON読み取りのETag条件 | ✅ 対象GET | ❌ | ❌ |
| バケットlabels・既定classのPATCH | ✅ | ❌ 指定を無視 | ◐ キー単位の削除なし |
| バケットlabels・既定classのPUT | ✅ | ❌ | ❌ |
| オブジェクトの標準・独自メタデータPATCH | ✅ | ◐ null削除なし | ◐ マップを置換 |
| オブジェクトの標準・独自メタデータPUT | ✅ | ◐ 一部項目を保持 | ❌ |

バケットのPATCH・PUTは省略項目を保持し、PUTは指定したlabelsのマップを全置換します。
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
| gRPC Storage v2を公式Goクライアントで使う（平文h2c。バケットCRUD・更新・IAM、オブジェクトの読み書き・一覧・更新・削除・復元、resumableの開始・照会・取消、範囲・双方向読み取り、rewrite・compose・move。通常バケットのみでRapid appendは除く） | ✅ JSON・XMLと状態・strict認可を共有 | ❌ Storage v1のみ | ❌ |
| 旧Storage v1 gRPC（比較対象の12 RPC。複数メッセージのInsertObject、プロジェクト省略のListBucketsを含む） | ✅ 記載したフィールド範囲 | ◐ InsertObjectは最初のメッセージのデータだけ | ❌ |

署名らしいURLを受け付けることと、署名・期限・権限を検証することを分けています。
IAMの保存やCORSの成功も、アクセス制御を保証しません。
gcs-mimicの公開主体判定はローカルのstrict IAM範囲で、ACLやUBLA遷移は含みません。

## ローカルでの使い方

| 同じ範囲で比較する操作 | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| APIで保存した本体を再起動後も保持 | ✅ | ✅ ファイル方式 | ✅ ディスク方式 |
| APIの保存先をバケット名・ファイル名で見る | ✅ `--sync-dir`で書き出し | ✅ ファイル方式 | ✅ ディスク方式 |
| 外部編集後にhash・世代も整合させる | 🚧 作成・編集を起動時に取り込み（削除・監視なし） | ❌ | ❌ |
| 任意のメモリ保存モード | ❌ | ✅ | ✅ |
| バケット・ファイルの初期ディレクトリ読込 | ✅ 毎回の起動時 | ✅ 起動・reseed | ❌ |
| 公開済みのDockerイメージ | ✅ GHCR・Docker Hub（amd64・arm64） | ✅ | ✅ |
| 公開済みの単体バイナリ | ✅ macOS・Windows・Linux | ✅ | ❌ Pythonが必要 |

fake-gcs-serverは実ファイルの直接編集がGETに反映される場合もありますが、
hashや世代が古いまま残ります。[保存方式の調査](storage-backends.md)を参照してください。
gcs-mimicは起動のたびにreadinessより前に初期ファイルを反映し、
イメージは追加の設定なしで`/data`を読み込みます。fake-gcs-serverと同じく、本体やメタデータが
初期ファイルと異なるオブジェクト（APIでのメタデータ変更を含む）は新しい現行世代になります。
初期ファイルから書き込む内容と完全に同じオブジェクトだけは世代を変えずに残しますが、
fake-gcs-serverはこの場合も新しい世代を書き込みます。fake-gcs-serverは`/_internal/reseed`も備えています。
gcs-mimicにはreseed用のエンドポイントもファイル監視もないため、編集の反映には再起動します。
どちらも、初期ファイルを消したオブジェクトは削除しません。

gcs-mimicの同期ディレクトリは、カタログとは別の写しで、2つ目の保存方式ではありません。
APIの変更は応答の少し後に現れ、外部の作成・編集は次の起動時にhashを計算し直した新しい世代になります。
外部削除、ファイル監視、競合の解決は未実装です。
[使い方と制約](container-usage.md#オブジェクトをホストのディレクトリへ書き出す)を参照してください。
ディスクへの保存は、停電耐性や同等の性能を保証しません。
同条件のベンチマークを実施していないため、速度・メモリ・サイズの順位は付けません。

## 検証の範囲

他製品は固定した版のソースと文書を調査しています。
fake-gcs-serverにはバケットAPI比較とmacOSのファイル・CORS確認を実行しましたが、
製品全体のテストや他製品でのブラウザー検証は実施していません。
gcs-mimicのブラウザー検証は、信頼したHTTPSを使うChromiumの宣言済み範囲です。

操作ごとの証跡・出典と省略した詳細は[英語の比較表](../en/feature-comparison.md)、
機能IDごとの状態は[生成された機能表](feature-matrix.md)を参照してください。
このページは台帳と別の実装状態を管理しません。
