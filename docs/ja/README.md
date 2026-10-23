# gcs-mimic

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

gcs-mimicは、ローカル開発とCIで使うGoogle Cloud Storageエミュレーターです。
Google Cloudのアカウントなしで、バケット作成やファイルの保存・取得を試せます。

[English](../../README.md) ·
[実装状態の原本](../../compatibility/features.json)

## まず動かす

Docker Composeとcurlを使います。ソースの取得、Rust、証明書の準備は不要です。

**イメージは未公開です。** 初回公開予定の
`ghcr.io/shiroemons/gcs-mimic:0.1.0`は、まだpullできません。
以下は公開後、またはローカルで検証したイメージに置き換えて使う手順です。

次を`compose.yaml`として保存します。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.1.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://localhost:4443
    volumes:
      - gcs-data:/var/lib/gcs-mimic
    healthcheck:
      test: [CMD, gcs-mimic, healthcheck, --url, "http://127.0.0.1:4443/_internal/ready"]
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
| ファイルを保存・取得・削除する | 対応 |
| コピー・継続rewrite・compose | 記載範囲で対応 |
| 再開可能アップロード・世代履歴 | 記載範囲で対応 |
| 公開URL・V4 RSA署名URLを使う | 記載範囲で対応 |
| 再起動後もデータを保持する | 対応 |
| 実フォルダとAPIを双方向同期する | 未実装 |
| 初期バケット設定をファイルで渡す | 未実装 |

CORSはバケットごとのXML用ルールと、JSON APIの固定ルールに分かれます。
メタデータは組み込みSQLite、ファイル本体は別ファイルへ保存します。
データベースサーバーは不要ですが、内部ファイル名はオブジェクト名と一致しません。
Google API全体やfake-gcs-serverとの完全互換を示すものではありません。

## 次に読む

- [Dockerでの使い方](container-usage.md)
- [機能比較](feature-comparison.md)と[詳細な機能表](feature-matrix.md)
- [設定・環境変数の対応](fake-gcs-server-configuration.md)
- [ファイル保存の仕組み](storage-backends.md)
- [ロードマップ](roadmap.md)と[並列開発の計画](delivery-plan.md)
- [バージョンと公開の方針](release-policy.md)
- [開発とpush前の確認](development.md)

HTTPS、SDK、認証の詳しい手順は
[英語の利用ガイド](../en/getting-started.md)を参照してください。

## ライセンス

Copyright (c) 2026 shiroemons

[MIT](../../LICENSE-MIT)または[Apache-2.0](../../LICENSE-APACHE)を選べます。
第三者の成果物には、それぞれのライセンスが適用されます。
本プロジェクトはGoogleの公式製品ではありません。
