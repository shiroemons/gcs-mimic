# 設定・環境変数の対応

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

fake-gcs-serverの設定は、名前を一括置換せず用途ごとに対応させます。
現在のgcs-mimicは`FAKE_GCS_*`を無視します。別名としては使えません。

[English](../en/fake-gcs-server-configuration.md) ·
[実装状態の原本](../../compatibility/features.json)

## fake-gcs-serverの19個の環境変数

調査対象はfake-gcs-server`v1.56.1`、コミット
`896ece51e49eed0946089f3089f6343855c0d106`です。
2026-10-24に確認した実行ファイルの設定を示します。
任意のCLIフラグから環境変数名を作る仕組みではありません。
Goライブラリのコンストラクターは、この設定層を自動利用しません。

| 環境変数 | CLI | 既定値 |
| --- | --- | --- |
| `FAKE_GCS_PORT` | `-port` | `4443` |
| `FAKE_GCS_PORT_HTTP` | `-port-http` | `8000` |
| `FAKE_GCS_BACKEND` | `-backend` | `filesystem` |
| `FAKE_GCS_FILESYSTEM_ROOT` | `-filesystem-root` | `/storage` |
| `FAKE_GCS_PUBLIC_HOST` | `-public-host` | `storage.googleapis.com` |
| `FAKE_GCS_EXTERNAL_URL` | `-external-url` | スキーム・ホスト・ポートから生成 |
| `FAKE_GCS_SCHEME` | `-scheme` | `https` |
| `FAKE_GCS_HOST` | `-host` | `0.0.0.0` |
| `FAKE_GCS_DATA` | `-data` | 空 |
| `FAKE_GCS_CORS_HEADERS` | `-cors-headers` | 追加ヘッダーなし |
| `FAKE_GCS_EVENT_PUBSUB_PROJECT_ID` | `-event.pubsub-project-id` | 空 |
| `FAKE_GCS_EVENT_PUBSUB_TOPIC` | `-event.pubsub-topic` | 空 |
| `FAKE_GCS_EVENT_BUCKET` | `-event.bucket` | 絞り込みなし |
| `FAKE_GCS_EVENT_OBJECT_PREFIX` | `-event.object-prefix` | 絞り込みなし |
| `FAKE_GCS_EVENT_LIST` | `-event.list` | `finalize` |
| `FAKE_GCS_LOCATION` | `-location` | `US-CENTRAL1` |
| `FAKE_GCS_CERT_LOCATION` | `-cert-location` | 空・組み込み証明書 |
| `FAKE_GCS_PRIVATE_KEY_LOCATION` | `-private-key-location` | 空・組み込み鍵 |
| `FAKE_GCS_LOG_LEVEL` | `-log-level` | `info` |

HTTPだけで起動する`-scheme http`でも、既定のポートは`4443`です。
`8000`は`-scheme both`で追加されるHTTPリスナー用です。
両方を使う場合、自動生成されるexternal URLはHTTPSを選びます。

公式イメージのentrypointは`-data /data`を渡します。
表の空という既定値とは異なり、公式イメージでは`/data`から初期読込します。

`-public-host`は公開URLのホストと、対象オブジェクトルートのHost照合に使います。
ポートを指定した場合は照合にも含まれます。待受アドレスやアクセス権は変えません。
`-external-url`は再開可能アップロードなどのAPI応答URL用で、末尾の`/`は除かれます。

`-cors-headers`は全体で許可するリクエストヘッダーの追加です。
バケットごとのCORSや、公開するレスポンスヘッダーの指定ではありません。
`-location`は全体のバケット作成時の既定値です。
イベント設定は1つの通知先と絞り込みを設定し、バケット別ポリシーを作りません。

## 優先順位と入力エラー

fake-gcs-serverは、未設定または空の環境変数に既定値を使います。
有効なCLI値が環境変数より優先され、同じフラグを繰り返した場合は最後を使います。
公式イメージではentrypointの`-data /data`が`FAKE_GCS_DATA`より優先されます。
変える場合はCLIに`-data /another-directory`を追加します。

両方のポート環境変数はCLIより先に解析します。数値でない値は、
CLIで上書きする場合や追加HTTPリスナーを使わない場合も起動エラーになります。
解析可能な範囲外の数値なら、有効なCLI値で置き換えてから最終検証できます。
未知の`FAKE_GCS_*`名は無視します。

ログレベルは`debug`、`info`、`warning`、`warn`、`error`です。
ヘルプにあっても`trace`、`fatal`、`panic`は受け付けません。
通知を有効にするにはprojectとtopicの両方が必要です。
通知種別は`finalize`、`delete`、`metadataUpdate`、`archive`で、
ヘルプにない`archive`も使えます。通知種別は通知有効時だけ検証します。

## gcs-mimicでの対応

gcs-mimicの優先順位はCLI、環境変数、サーバーJSON設定、既定値の順です。
未知の`GCS_MIMIC_*`はエラーになり、空の値が常に既定値扱いになるわけではありません。
`FAKE_GCS_*`を見つけて移行方法を表示する診断は、まだ実装していません。

| 用途 | 現在の設定 | 違い |
| --- | --- | --- |
| HTTP・HTTPS | `GCS_MIMIC_SCHEME` | 同時待受は未対応 |
| 待受ホスト・ポート | `GCS_MIMIC_LISTEN` | `IP:port`を1つ指定 |
| API応答に載せる接続先 | `GCS_MIMIC_API_BASE_URL` | 公開URLの生成とは別 |
| 初期ファイル | `GCS_MIMIC_SEED_DIR` | 初回読込のみ |
| 初期バケットのproject | `GCS_MIMIC_SEED_PROJECT` | 所有projectを指定 |
| 永続データ保存先 | `GCS_MIMIC_DATA_DIR` | 旧保存先の直接流用は不可 |
| 証明書 | `GCS_MIMIC_TLS_CERT` | 対応する鍵も必要 |
| 秘密鍵 | `GCS_MIMIC_TLS_KEY` | 対応する証明書も必要 |
| 公開URLの生成 | `url public --base-url` | 単体ではHost照合を設定しない |
| 仮想ホストのルート | `GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS` | 同じHost照合ではない |
| バケットCORS | JSONバケットAPI | XML用・JSONは別の固定ルール |
| バケットlocation | JSONバケットAPI | `US`・`EU`・`ASIA` |
| メモリ保存モード | なし | 未対応 |
| イベント通知先・絞り込み | なし | 未対応 |
| 全体のlocation既定値 | なし | 未対応 |
| ログレベル選択 | なし | 未対応 |

ネイティブCLIは`serve --scheme http`などの形式です。
全項目のCLI・既定値は
[英語の設定リファレンス](../en/getting-started.md#native-binary-and-configuration)、
用途別の起動例は[移行ガイド](../en/fake-gcs-server-migration.md#startup-options-and-current-equivalents)
を参照してください。

初期ファイルの読込では、元のMIME・メタデータをそのまま保持するわけではありません。
詳細は[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。

`-public-host`に相当する単一設定はありません。
URLヘルパー、API応答用URL、仮想ホストルート、権限を分けて扱います。
strict署名は最終的にクライアントが使うURLへ署名し、後からホストを書き換えません。
`legacy-emulator`の固定`US-CENTRAL1`応答も、locationの自由な設定を意味しません。

リポジトリのComposeで使う`GCS_MIMIC_PORT`はホスト側の公開ポート用です。
サーバー自身の環境変数ではなく、バイナリへ渡しても待受設定にはなりません。

## 根拠と検証範囲

Go`1.27.3`で固定版の設定パッケージのテストと個別確認を実行しています。
19個の値と、14件の優先順位・入力検証にはentrypointの引数も含みます。
詳細は[英語の設定検証記録](../en/evidence/fake-gcs-server-configuration.md)にあります。
全上流テスト、実際のPub/Sub通知、証明書の読込までは検証していません。

不足機能の実装順は[ロードマップ](roadmap.md)で管理します。
