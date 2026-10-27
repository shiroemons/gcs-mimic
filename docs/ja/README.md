# gcs-mimic

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

gcs-mimicは、ローカル開発とCIで使うGoogle Cloud Storageエミュレーターです。
Google Cloudのアカウントなしで、バケット作成やファイルの保存・取得を試せます。

[English](../../README.md) ·
[実装状態の原本](../../compatibility/features.json)

## まず動かす

Docker Composeとcurlを使います。ソースの取得、Rust、証明書の準備は不要です。

イメージ`ghcr.io/shiroemons/gcs-mimic:0.1.3`は`linux/amd64`と`linux/arm64`向けに公開済みです。
[0.1.3のリリースノート（英語）](../en/releases/0.1.3.md)に今回の公開内容、
[0.1.0のリリースノート（英語）](../en/releases/0.1.0.md)に機能と既知の制限を記載しています。
ソースからビルドする場合は[ソースからの導入手順（英語）](../en/getting-started.md)を使ってください。

次を`compose.yaml`として保存します。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.1.3
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://localhost:4443
    volumes:
      - gcs-data:/var/lib/gcs-mimic
volumes:
  gcs-data:
```

起動してバケットを作り、`hello`を保存して読み出します。

```sh
docker compose up -d --wait
curl --fail \
  -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl --fail 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

最後のコマンドで`hello`が表示されます。この例では認証を要求しません。
`docker compose down`で停止しても、名前付きボリュームにデータが残ります。

fake-gcs-serverの初期ファイルがある場合は、同じ`data/BUCKET/OBJECT`配置を使えます。
[初期ファイルの読み込み](container-usage.md#初期ファイルを読み込む)を参照してください。

## できることと制約

| 用途 | 現在の対応 |
| --- | --- |
| バケットを複数作る | 対応 |
| バケットごとにCORS・IAMを設定する | APIで設定可能 |
| UBLAを有効にしてバケットを作る | Go・Rubyの通常の呼び方に対応。[対応範囲とACLの制限](sdk-ubla-predefined-acl.md) |
| ファイルを保存・取得・削除する | 対応 |
| コピー・継続rewrite・compose | 記載範囲で対応 |
| 再開可能アップロード・世代履歴 | 記載範囲で対応 |
| 公開URL・V4 RSA署名URLを使う | 記載範囲で対応 |
| 再起動後もデータを保持する | 対応 |
| 実フォルダとAPIを双方向同期する | 未実装 |
| 初期バケット設定をファイルで渡す | JSON v1で未作成バケットだけ作成 |
| fake-gcs-serverのファイルシステムのデータを取り込む | ライブのオブジェクトだけ（履歴は未対応） |
| gRPCを使う | 未実装 |
| 公開Dockerイメージをpullする | `0.1.3`（amd64・arm64） |

CORSはバケットごとのXML用ルールと、JSON APIの固定ルールに分かれます。
メタデータは組み込みSQLite、ファイル本体は別ファイルへ保存します。
データベースサーバーは不要ですが、内部ファイル名はオブジェクト名と一致しません。
Google API全体やfake-gcs-serverとの完全互換を示すものではありません。
一度だけ読み込む初期ファイルと、既存バックエンドの状態の取り込みは別の機能です。
既知19個の`FAKE_GCS_*`環境変数は移行案内付きのエラーになり、未知の名前は警告されます。
[対応する設定](fake-gcs-server-configuration.md)を確認し、`GCS_MIMIC_*`を指定してください。

stderrの運用ログはJSONです。`--log-level`または`GCS_MIMIC_LOG_LEVEL`で
`debug`・`info`・`warn`・`error`を指定でき、既定値は`info`です。
設定の診断とstdoutの`ready`・`stopped`は、どのレベルでも表示します。

## 次に読む

- [Dockerでの使い方](container-usage.md)
- [機能比較](feature-comparison.md)と[詳細な機能表](feature-matrix.md)
- [設定・環境変数の対応](fake-gcs-server-configuration.md)
- [ファイル保存の仕組み](storage-backends.md)
- [ロードマップ](roadmap.md)と[並列開発の計画](delivery-plan.md)
- [バージョンと公開の方針](release-policy.md)
- [0.1.0のリリースノートと既知の制限（英語）](../en/releases/0.1.0.md)
- [開発とpush前の確認](development.md)

HTTPS、SDK、認証の詳しい手順は
[英語の利用ガイド](../en/getting-started.md)を参照してください。

## ライセンス

Copyright (c) 2026 shiroemons

[MIT](../../LICENSE-MIT)または[Apache-2.0](../../LICENSE-APACHE)を選べます。
第三者の成果物には、それぞれのライセンスが適用されます。
本プロジェクトはGoogleの公式製品ではありません。
