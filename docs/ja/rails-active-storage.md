# Rails Active Storageでgcs-mimicを使う

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

[English](../en/rails-active-storage.md)

開発・テストではgcs-mimic、本番ではGoogle Cloud Storageを使います。本番の設定は変えません。

Rails 8.1.4、google-cloud-storage 1.62.1、googleauth 1.17.6と1.17.7で確認しています（[検証結果](sdk-active-storage-scenario.md#storageymlだけでエミュレーターを使う)）。
ほかの版は検証していません。

## 1. バケット付きでエミュレーターを起動する

`buckets.json`で、起動時にバケットを作ります。
CORSの設定で、ブラウザがアプリのオリジンからダイレクトアップロードできるようにします。
[Railsガイド](https://guides.rubyonrails.org/active_storage_overview.html)の
Google Cloud Storage向けの設定に、`Cache-Control`を足したものです。
サービスに`cache_control`を設定するとブラウザがこのヘッダーを送るため、ないとそのアップロードはCORSで失敗します。
`origin`は自分のアプリのオリジンにします。

```json
{"version": 1, "project": "local-project", "buckets": [
  {"name": "local-bucket", "cors": [{
    "origin": ["http://localhost:3000"], "method": ["PUT"],
    "responseHeader": ["Content-Type", "Content-MD5", "Content-Disposition", "Cache-Control"],
    "maxAgeSeconds": 3600}]}
]}
```

```yaml
# compose.yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.6.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - ./buckets.json:/buckets.json:ro
```

## 2. `config/storage.yml`にエミュレーター用の項目を足す

本番用の項目は残し、その横に`gcs_emulator`を足します。

```yaml
google: # production, unchanged
  service: GCS
  project: my-project
  bucket: my-bucket
  credentials: <%= Rails.root.join("path/to/keyfile.json") %>

gcs_emulator: # development and test
  service: GCS
  project: local-project
  bucket: <%= ENV.fetch("GCS_EMULATOR_BUCKET", "local-bucket") %>
  endpoint: <%= ENV.fetch("GCS_EMULATOR_ENDPOINT", "http://localhost:4443/") %>
  universe_domain: gcs-emulator.invalid
  credentials:
    type: service_account
    project_id: local-project
    client_email: local@example.invalid
    universe_domain: gcs-emulator.invalid
    private_key: <%= OpenSSL::PKey::RSA.generate(2048).to_pem.dump if Rails.env.local? %>
```

- **`universe_domain`で、RailsがGoogleに接続しなくなります。** これはGoogle Cloudの公開クラウド以外のクラウド向けの設定です。`googleapis.com`以外の値にすると、googleauthはGoogleのOAuthにトークンを取りに行かず、自分でトークンに署名します。これがないと、アップロードのたびに`oauth2.googleapis.com`へ接続しようとして失敗します。2か所に同じ値を書いてください。
- **鍵は使い捨てです。** 開発・テストのときだけ起動時に生成し、ローカルのURLの署名に使います。Googleの認証情報ではありません。
- `endpoint`の末尾の`/`は残します。

## 3. 環境ごとに項目を選ぶ

```ruby
# config/environments/development.rb and config/environments/test.rb
config.active_storage.service = :gcs_emulator

# config/environments/production.rb
config.active_storage.service = :google
```

## 4. Railsをコンテナで動かす場合

RailsはComposeのサービス名でエミュレーターに届きますが、手元のブラウザはその名前を解決できません。
そのため、Railsがブラウザをエミュレーターへリダイレクトする画像は表示されません。
ダイレクトアップロードも`Error storing ... Status: 0`で失敗します。

両方で`gcs.localhost`という名前を使います。
ブラウザは`*.localhost`を自分のマシンに送り、ComposeのネットワークエイリアスはDocker内でエミュレーターに送るので、Railsとブラウザが同じURLを使えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.6.0
    ports:
      - "127.0.0.1:4443:4443" # the host port must equal the container port
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://gcs.localhost:4443
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - ./buckets.json:/buckets.json:ro
    networks:
      default:
        aliases:
          - gcs.localhost
  web: # your Rails service
    environment:
      GCS_EMULATOR_ENDPOINT: http://gcs.localhost:4443/
```

これで、画像の表示、ダイレクトアップロード、Rails自身のアップロードがすべて動きます。
同じポート番号で公開できない場合は、Rails経由でファイルを配信します。ダイレクトアップロードには、ブラウザから届く`endpoint`が引き続き必要です。

```ruby
# config/environments/development.rb
config.active_storage.resolve_model_to_route = :rails_storage_proxy
```

## 5. CI

エミュレーターをサービスとして起動し、準備ができるのを待ってからバケットを作ります。
起動直後はポートが空の応答を返すことがあり、`--retry-connrefused`ではこれを再試行しないため、`--retry-all-errors`で待ちます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.6.0
    env:
      GCS_MIMIC_SCHEME: http
    ports:
      - 4443:4443
steps:
  - run: >
      curl -sS --fail --retry 30 --retry-all-errors --retry-delay 1
      -o /dev/null http://localhost:4443/_internal/ready
  - run: >
      curl -sS --fail -H 'Content-Type: application/json'
      -d '{"name":"local-bucket"}'
      'http://localhost:4443/storage/v1/b?project=local-project'
```

これらのコマンドは、GitHubのホステッドランナーと同じくホストにポートを公開したイメージに対して確認しました。ワークフロー自体はGitHub上では実行していません。

## 確認済みのこと

リポジトリのテストで、`has_one_attached`のモデル経由で、エミュレーター以外への通信を止めるガードの下で確認しています。

| 動くこと | 確認方法 |
| --- | --- |
| アップロード・ダウンロード・`purge` | 本文を比較し、エミュレーターにオブジェクトがあることを確認 |
| `url`と既定のリダイレクト | 署名付きURLで本文が返り、リダイレクト先が`endpoint`になる |
| プロキシ配信 | リダイレクトせず、Railsがファイルを返す |
| ダイレクトアップロード（V2と、`cache_control`時のV4） | RailsのURLとヘッダー、ブラウザと同じCORSのプリフライトと`PUT`。許可していないオリジンは拒否 |
| GoogleのOAuthに接続しない | 起動から400日後のアップロードもローカルで完結 |
| 本番に影響しない | 本番の起動では本番用の項目が選ばれ、鍵も生成されない |

Pumaで動かしたRailsアプリでも、Chromeで確認しました。Railsをホストで動かす場合と、`gcs.localhost`を使ってComposeで動かす場合です（空いている別のポートを使い、それ以外はこのページのとおり）。
画像の表示、Active StorageのJavaScriptによるダイレクトアップロード、Rails経由のアップロードがすべて動きました。ほかのブラウザでは試していません。

制限:

- 既定のpermissiveプロファイルでだけ使えます。
  `GCS_MIMIC_AUTH_MODE=strict`では、アップロードが`401 Invalid Credentials`で失敗します。
  strictモードはIDファイルに登録したトークンしか受け付けないためです。
  このとき、エミュレーターのログに`auth_self_signed_jwt_rejected`の警告が1回出ます。
- googleauthが`googleapis.com`以外で自分でトークンに署名する動作に依存しています。googleauth 1.17.6と1.17.7で確認しました。版を固定し、更新後に確認し直してください。より簡単なエミュレーター設定を追加する[google-cloud-ruby#34023](https://github.com/googleapis/google-cloud-ruby/pull/34023)は、2026-10-30時点で未マージです。

多くのRailsアプリは、開発・テストでDiskサービスを使っています。そのほうが簡単ですが、署名付きURLやCORSなど、Google Cloud Storageの動きは確認できません。
