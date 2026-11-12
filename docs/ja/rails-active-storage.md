# Rails Active Storageでgcs-mimicを使う

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

[English](../en/rails-active-storage.md)

開発・テストではgcs-mimic、本番ではGoogle Cloud Storageを使います。本番の設定は変えません。

Rails 8.1.4.1、google-cloud-storage 1.62.1、googleauth 1.17.6で確認しています（[検証結果](sdk-active-storage-scenario.md#storageymlだけでエミュレーターを使う)）。
Rails 8.1.4はgoogleauth 1.17.7でも確認しています。ほかの版は検証していません。

Rails 8.1.4.1のダイレクトアップロードには、gcs-mimic 0.10.0以降が必要です。
Rails 8.1.4.1はすべてのダイレクトアップロードURLに`x-goog-content-length-range`を署名して含めます。
0.10.0はこの範囲を検査します。0.9.0はこのヘッダーを`400 InvalidArgument`で拒否します。
詳しくは[XMLアップロードのサイズ範囲](../en/slices/xml-content-length-range.md)（英語）を見てください。

## 1. バケット付きでエミュレーターを起動する

`buckets.json`で、起動時にバケットを作ります。
CORSの設定で、ブラウザがアプリのオリジンからダイレクトアップロードできるようにします。
[Railsガイド](https://guides.rubyonrails.org/active_storage_overview.html)の
Google Cloud Storage向けの設定に、`Cache-Control`を足したものです。
サービスに`cache_control`を設定するとブラウザがこのヘッダーを送るため、ないとそのアップロードはCORSで失敗します。
Rails 8.1.4.1は`x-goog-content-length-range`も送るため、これも許可します。
`origin`は自分のアプリのオリジンにします。

```json
{"version": 1, "project": "local-project", "buckets": [
  {"name": "local-bucket", "cors": [{
    "origin": ["http://localhost:3000"], "method": ["PUT"],
    "responseHeader": ["Content-Type", "Content-MD5", "Content-Disposition", "x-goog-content-length-range", "Cache-Control"],
    "maxAgeSeconds": 3600}]}
]}
```

```yaml
# compose.yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.19.1
    ports:
      - "127.0.0.1:4443:4443"
    environment:
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
- **鍵は使い捨てです。** 開発・テストのときだけ起動時に生成し、ローカルのURLの署名に使います。Googleの認証情報ではありません。エミュレーターが知らない鍵のため、strictでは拒否されます。[手順6](#6-strictで本番に近い確認をする)では、代わりに組み込みのテスト署名者を使います。
- `endpoint`の末尾の`/`は残します。
- **`public: true`には回避策が必要です。** そのURLはエミュレーターに届きません。[公開ファイル](#公開ファイル)を参照してください。

### 公開ファイル

`public: true`のサービスでは、Railsは各ファイルを公開URLでリンクします。
google-cloud-storage 1.62.1はこのURLを`endpoint`にかかわらず常に`https://storage.googleapis.com`で組み立てます。
そのため、ローカルのアプリの公開画像はエミュレーターから読み込まれません。
次のどちらの回避策でも、このURLをエミュレーターに向け、本番には影響しません。
strictでは、[strictのローカル構成（英語）](../../examples/strict-local/README.md)のように、バケットに`allUsers`の`iamPolicy`も必要です。

1. **推奨: `EmulatorGcs`アダプター。** [手順6](#6-strictで本番に近い確認をする)のとおりアダプターをコピーします。
   配布しているファイルには、次の`public_url`メソッドがすでに入っています。Googleの代わりに、ブラウザーのオリジンである`public_endpoint`を使います。

   ```ruby
   # In lib/active_storage/service/emulator_gcs_service.rb, which also requires "erb".
   private

     # The SDK names storage.googleapis.com in every public URL whatever the endpoint is; a public
     # bucket serves the same path on the final origin.
     def public_url(key, **)
       path = key.split("/", -1).map { |segment| ERB::Util.url_encode(segment) }.join("/")
       "#{@public_endpoint.to_s.delete_suffix('/')}/#{bucket.name}/#{path}"
     end
   ```

   ローカルの`storage.yml`の項目だけで、`public: true`を付けて選びます。本番は`service: GCS`と本来のURLのままです。

   ```yaml
   gcs_emulator_public:
     service: EmulatorGcs
     public: true
     public_endpoint: <%= ENV.fetch("GCS_EMULATOR_PUBLIC_ENDPOINT", "http://localhost:4443") %>
     # project, bucket, endpoint, universe_domain and credentials as in step 6
   ```

   選んだ項目にだけ効くので、本番の項目が変わることはありません。`public_endpoint`を使うので、Railsが使うアドレスとブラウザーのオリジンが違う場合にも対応できます。

2. **別の方法: initializer。** ローカルでも標準の`service: GCS`を使う場合は、次のファイルを`config/initializers/active_storage_gcs_emulator.rb`として追加します。
   `Rails.env.local?`（developmentとtest）のときだけ、しかも`storage.yml`の項目に`endpoint`を書いたサービス（エミュレーター用の項目など）にだけ効きます。本番の項目には`endpoint`がないので、標準のURLのままです。

   ```ruby
   # frozen_string_literal: true

   # Active Storage builds the URLs of a public: true GCS service with the SDK, which always names
   # storage.googleapis.com. In development and test, a service whose storage.yml entry sets an
   # endpoint, such as the gcs-mimic entry, names that endpoint instead. Entries without an endpoint,
   # such as production's, keep the stock URL.
   if Rails.env.local?
     require "active_storage/service/gcs_service"
     require "erb"

     module GcsEmulatorPublicUrl
       private

       def public_url(key, **)
         endpoint = config[:endpoint]
         return super if endpoint.blank?

         path = key.split("/", -1).map { |segment| ERB::Util.url_encode(segment) }.join("/")
         "#{endpoint.delete_suffix('/')}/#{bucket.name}/#{path}"
       end
     end

     ActiveStorage::Service::GCSService.prepend(GcsEmulatorPublicUrl)
   end
   ```

   項目の`endpoint`をブラウザーのアドレスとして使うので、Railsとブラウザーが同じURLを使う構成（`localhost:4443`や、Composeでの`gcs.localhost`）に向いています。2つのアドレスが違う場合はアダプターを使います。

どちらも、Rails 8.1.4.1のprivateな`public_url(key, **)`を前提にしています。
strictのテストは両方を実行しています。
標準のままでは`storage.googleapis.com`を指すこと、どちらの回避策でもエミュレーターの正確なURLになり、`allUsers`のバケットから認証なしで読めることを確かめています。
`resolve_model_to_route = :rails_storage_proxy`は、この問題の解決策にはなりません。
`blob.url`はGoogleを指したままです。

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
ポートも両方で同じにする必要があるため、エミュレーターはコンテナ内でも公開するポートで待ち受けます。
そのポートは`GCSM_PORT`で選べるので、ワークツリーやプロジェクトごとに別のポートを使えます。

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.19.1
    ports:
      # The host port equals the container port, so one URL works on both sides.
      - "127.0.0.1:${GCSM_PORT:-4443}:${GCSM_PORT:-4443}"
    environment:
      GCS_MIMIC_LISTEN: 0.0.0.0:${GCSM_PORT:-4443}
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - ./buckets.json:/buckets.json:ro
    networks:
      default:
        aliases:
          - gcs.localhost
  web: # your Rails service
    environment:
      GCS_EMULATOR_ENDPOINT: http://gcs.localhost:${GCSM_PORT:-4443}/
```

4443番が使用中なら、ワークツリーの`.env`などで`GCSM_PORT`を設定します。Composeがファイル内で置き換えます。
Composeネットワーク上のほかのサービスも、`http://gcs:${GCSM_PORT:-4443}`でエミュレーターに届きます。
Railsのポートもワークツリーごとに変わる場合は、バケット文書のCORSのoriginもそれに合わせる必要があります。
0.14.0以降では、`GCS_MIMIC_BUCKETS_FILE`の代わりに`GCS_MIMIC_BUCKETS`へ文書をインラインで書くと、Composeがそこでもポートを置き換えます。
例えば、自分で決めた変数を使って`"origin": ["http://localhost:${RAILS_PORT:-3000}"]`と書きます。
インラインの文書は、このような変更を次の起動時に既存のバケットにも反映します。

これで、画像の表示、ダイレクトアップロード、Rails自身のアップロードがすべて動きます。
ホスト側とコンテナ側を同じポート番号にできない場合は、Rails経由でファイルを配信します。ダイレクトアップロードには、ブラウザから届く`endpoint`が引き続き必要です。

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
    image: ghcr.io/shiroemons/gcs-mimic:0.19.1
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

## 6. strictで本番に近い確認をする

手順2の項目は起動時に生成した鍵で署名するため、既定のpermissiveプロファイルでしか受け付けられません。
strictはCloud Storageと同じようにすべてのリクエストを認可します。署名付きURLなしで非公開オブジェクトを要求したブラウザーは401を受け取り、Railsにはエミュレーターが知っている鍵が必要です。
Railsには、gcs-mimicに同梱している組み込みのテスト署名者`test-signer@gcs-mimic.invalid`の鍵を渡します。鍵の生成も登録も不要です。
0.14.0以降が必要です。

手順1または手順4の`gcs`サービスに、設定を2つ足します。

```yaml
    environment:
      GCS_MIMIC_AUTH_MODE: strict
      GCS_MIMIC_TEST_SIGNER: enabled
```

この場合、strictでもID文書はいりません。
署名者には、バケット文書が作るバケットのプロジェクトと`GCS_MIMIC_SEED_PROJECT`（既定は`local-project`）に対する`roles/storage.objectUser`が付きます。
このロールで下に挙げたRailsの操作はすべて動きますが、バケットは作れません。そのため、アプリのバケットは手順1のバケット文書で作ります。
起動時に、署名者の秘密鍵が公開されていることを警告します。
別のロールを付けるには、`GCS_MIMIC_IDENTITIES`または`GCS_MIMIC_IDENTITY_FILE`に`test-signer@gcs-mimic.invalid`を`project_roles`付きで宣言します。宣言すると、`GCS_MIMIC_TEST_SIGNER`なしでも署名者が有効になります。

署名者の鍵を、サービスが使うイメージから取り出してアプリに保存します。

```sh
docker compose run --rm gcs test-signer credentials | jq -r .private_key > config/gcs-mimic-test-signer.pem
```

手順2の`gcs_emulator`の認証情報を書き換えます。

```yaml
gcs_emulator: # development and test
  service: GCS
  project: local-project
  bucket: <%= ENV.fetch("GCS_EMULATOR_BUCKET", "local-bucket") %>
  endpoint: <%= ENV.fetch("GCS_EMULATOR_ENDPOINT", "http://localhost:4443/") %>
  universe_domain: gcs-emulator.invalid
  credentials:
    type: service_account
    project_id: local-project
    client_email: test-signer@gcs-mimic.invalid
    universe_domain: gcs-emulator.invalid
    private_key: <%= File.read(Rails.root.join("config/gcs-mimic-test-signer.pem")).dump if Rails.env.local? %>
```

`credentials`には、`test-signer credentials`が出力する項目を書きます。
Google以外の`universe_domain: gcs-emulator.invalid`も含むため、googleauthはGoogleのOAuthに問い合わせず、公開済みの鍵で自分でトークンに署名します。
鍵は公開されていて、誰でもこのIDで署名できます。ローカルの開発とテストだけで使い、共有サーバーやGoogleの認証情報には使いません。
permissiveではトークンを無視するため、同じ項目がpermissiveでも動きます。

**どの待受を使うか。** エミュレーターがほかのサーバー向けに[信頼済みリスナー](container-usage.md#信頼済みリスナーで非公開バケットを守る)を持つ場合でも、`endpoint`にはブラウザーが届くメインの待受を指定します。
Railsは常に自分のトークンを送るため、どちらの待受でも署名者として認証されます。
ただし、`endpoint`はRailsがブラウザーに渡すURL（`url`のリダイレクト先とダイレクトアップロードのURL）のoriginにもなります。
信頼済みリスナーのポートは公開せず、ブラウザーからのリクエストも拒否します。
Railsをコンテナで動かす場合は、手順4のとおり`gcs.localhost`を使います。

**V2とV4のURL。** 標準の`service: GCS`は、ダウンロードとダイレクトアップロードにV2のURLを署名します。
V2の署名はhostを含まないため、これらのURLは`endpoint`が指すどのアドレスでも検証に通ります。ワークツリーごとのポートを使う`gcs.localhost`も同じです。
V4のURLを使う場合は、`EmulatorGcs`アダプターを選びます。
[emulator_gcs_service.rb](../../sdk-tests/ruby/active_storage/lib/active_storage/service/emulator_gcs_service.rb)をアプリの`lib/active_storage/service/`にコピーし、`lib`をロードパスに含め、`public_endpoint`にブラウザーが使うoriginだけを指定します。

```yaml
gcs_emulator:
  service: EmulatorGcs
  public_endpoint: <%= ENV.fetch("GCS_EMULATOR_PUBLIC_ENDPOINT", "http://localhost:4443") %>
  # project, bucket, endpoint, universe_domain and credentials as above
```

アダプターはURLを出力する前に`public_endpoint`のhostとportを署名するため、`signed_host_port=lenient`なしで検証に通ります。署名後にhostを書き換えません。
ダイレクトアップロードでは`x-goog-content-length-range`も署名し、サービスに`cache_control`を設定した場合は`Cache-Control`も署名します。
Railsをコンテナで動かす場合は、`endpoint`と`public_endpoint`のどちらも`gcs.localhost:${GCSM_PORT:-4443}`を指します。

strictのRailsテスト（`sdk-tests/ruby/active_storage/run-strict.sh`）は、同じ認証情報を使い、署名者が`roles/storage.objectUser`だけを持つstrictのサーバーに対して実行しています。
JWTによるアップロードとダウンロード、V2の署名付きGET、標準のV2と`EmulatorGcs`アダプターのV4によるダイレクトアップロード、
attach、variantsのprefixを含む`purge`、`delete_prefixed`、`blob.update!`による`content_type`の変更、`Blob.compose`、公開バケットを確認しています。
Goのサーバー・ブラウザー・Railsを含むアプリ全体では、[strictのローカル構成（英語）](../../examples/strict-local/README.md)から始めてください。

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

上のレシピはpermissive用です。0.8.0以降は、登録したテスト用公開鍵で
[ローカルJWTを検証するstrict手順（英語）](../en/slices/strict-service-account-jwt.md)を使えます。
固定したローカル署名鍵を使い、対応する公開鍵・ID・権限を登録します。
起動ごとに生成した未登録の鍵ではstrict認証できません。
鍵を登録する代わりに、[組み込みのテスト署名者（英語）](../en/signed-urls.md#built-in-test-signer)も使えます。
エミュレーターを`GCS_MIMIC_TEST_SIGNER=enabled`で起動すると、バケット文書が作るバケットのprojectと`seed_project`では、権限を付けなくてもオブジェクトを読み書きできます。
ID文書に`test-signer@gcs-mimic.invalid`をプロジェクトロール付きで宣言しても有効になり、その場合はロールを自分で選べます
（strictではそれ以外は無効で、有効にすると起動時に警告が出ます）。ほかのprojectのバケットには、そのIDにバケットのIAMで権限を付けます。
`client_email`を`test-signer@gcs-mimic.invalid`に、`private_key`を公開済みの鍵にします。
秘密鍵は公開されていて誰でもこのIDで認証できるため、ローカルのテストだけで使います。
鍵はエミュレーター自身から取り出せるため、リポジトリのチェックアウトは不要です。

```sh
docker run --rm ghcr.io/shiroemons/gcs-mimic:IMAGE_TAG test-signer credentials \
  | jq -r .private_key > config/gcs-mimic-test-signer.pem
```

0.13.0にはこのコマンドがないため、0.14.0以降のタグを使います。
ファイルは[署名者の`storage.yml`の例（英語）](../en/signed-urls.md#built-in-test-signer)のように読み込みます。
同じ認証情報を使えば、permissiveでも`GCS_MIMIC_SIGNED_URLS=verify`でRailsの署名付きURLを検証できます。
起動時に生成した鍵はエミュレーターが知らないため、そのURLは`InvalidAccessKeyId`で失敗します。
V4でブラウザー向けの最終host・portを署名する場合は、明示的に`EmulatorGcs`アダプターを選びます。
署名後にhostを書き換えません。`just check-active-storage-strict`は2つの構成で実行し、どちらも7ケースすべてが成功しました。
1つは鍵を登録する構成、もう1つは`roles/storage.objectUser`だけを持つ組み込みのテスト署名者の構成です（[手順6](#6-strictで本番に近い確認をする)）。
匿名の非公開読込と署名ヘッダー改変の拒否も維持しています。
これはRailsモデルとNet::HTTPの検証で、strictの実RailsブラウザーUIは未確認です。
GoogleのOAuth発行・検証を再現する機能ではありません。

- 上のレシピは既定のpermissiveプロファイルで使います。
  `GCS_MIMIC_AUTH_MODE=strict`では、アップロードが`401 Invalid Credentials`で失敗します。
  strictは登録していない自己署名JWTを受け付けないためです（0.7.0以前は自己署名JWTを検証しません）。
  0.8.0以降のstrictは、明示した登録済みJWTの範囲だけを受け付けます。
  組み込みのテスト署名者がこの範囲に含まれるのは、ID文書がそれを宣言しているときか、`GCS_MIMIC_TEST_SIGNER=enabled`のときだけです。
  このとき、エミュレーターのログに`auth_self_signed_jwt_rejected`の警告が1回出ます。
  テスト署名者が無効なまま`test-signer credentials`の鍵で署名したJWTも401になります。このときの警告は、テスト署名者が無効であることと、`GCS_MIMIC_TEST_SIGNER=enabled`で有効にできることを伝えます。
- googleauthが`googleapis.com`以外で自分でトークンに署名する動作に依存しています。googleauth 1.17.6と1.17.7で確認しました。版を固定し、更新後に確認し直してください。より簡単なエミュレーター設定を追加する[google-cloud-ruby#34023](https://github.com/googleapis/google-cloud-ruby/pull/34023)は、2026-10-30時点で未マージです。

多くのRailsアプリは、開発・テストでDiskサービスを使っています。そのほうが簡単ですが、署名付きURLやCORSなど、Google Cloud Storageの動きは確認できません。
