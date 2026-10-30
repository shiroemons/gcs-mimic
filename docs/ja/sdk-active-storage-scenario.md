# Rails Active Storage の動作確認

<!-- markdownlint-configure-file {"MD013": false} -->

対象はDESIGN-084、状態はPARTIALです。専用のGemfileでActive Storageとrailties
8.1.4、sqlite3 2.10.2、google-cloud-storage 1.62.1を固定しています。
Rails全体や外部DB、画像変換ツールは追加していません。
製品のV2署名対応は[別スライス](sdk-active-storage-v2-signing.md)に記録しています。

## 実行方法

Ruby 4.0.9とBundler 4.0.23を用意し、専用の依存をインストールします。
Rust、Go、jq、justも必要です。Goは既存の空きポート取得プログラムで使います。

```sh
cd sdk-tests/ruby/active_storage
BUNDLE_FROZEN=true bundle install
cd ../../..
just check-active-storage
```

HTTPの隔離サーバーを起動して8ケースを個別に記録し、失敗しても残りを実行します。
最後にサーバーと一時データを片付けます。`just check-sdk`には含めていません。

`config/storage.yml`は通常の`service: GCS`にproject、bucket、ローカルの署名用credentials、
`endpoint: http://127.0.0.1:4443/`を指定する形です。SDKが末尾の`/`を必要とします。
接続先とバケット名は実行スクリプトが設定します。RailsのDBはインメモリSQLiteです。
テスト用の公開済み秘密鍵を実際の認証に使わないでください。

最初に、認証設定を変更せずにアップロードするAS-DEFAULT-AUTHを実行します。
googleauthがOAuthサーバーへ接続しようとするため、通信前にガードで止めてFAILと記録します。
その後だけ、内部クライアントのauthorizationを外して各操作を検証します。
この回避策の下での成功はPASS_WITH_WORKAROUNDと表示し、既定の呼び方のPASSに数えません。
接続は指定したループバックHTTP端点に制限し、プロキシやリダイレクトを無効にします。
Googleやメタデータサーバーへの接続、URL生成後のホスト置換は行いません。

## 確認結果

2026-10-27の従来7ケースの結果は次のとおりです。

| ケース | 結果 | 確認した内容 |
| --- | --- | --- |
| AS-DEFAULT-AUTH | FAIL | 認証を変えないアップロードがoauth2.googleapis.comへ接続を試みるため通信前に拒否 |
| AS-UPLOAD | PASS_WITH_WORKAROUND | 認証を外した条件でcontent_type省略のcreate_and_upload!とバイナリの保存・取得 |
| AS-DOWNLOAD | PASS_WITH_WORKAROUND | 認証を外した条件でblob.downloadとチェックサムを検証するblob.open |
| AS-DELETE | PASS_WITH_WORKAROUND | 認証を外した条件でblob.purge、variantプレフィックス削除、隣接プレフィックスの保護 |
| AS-SIGNED-URL | PASS_WITH_WORKAROUND | 認証を外した条件でblob.urlを変更せずGETし、保存内容と一致 |
| AS-DIRECT-UPLOAD | PASS_WITH_WORKAROUND | 同じ条件でサービスのURLとヘッダーを変更せずPUTし、保存内容と一致 |
| AS-EXIST-COMPOSE | PASS_WITH_WORKAROUND | 認証を外した条件で存在・不在の判定、順序を保つ合成、元オブジェクトの保持 |

合計は0 PASS・6 PASS_WITH_WORKAROUND・1 FAILで、コマンドは終了コード1を返します。
後続の[ローカル資格情報の検証](../en/slices/sdk-local-auth-softdelete-notes.md)では、
アプリ側でトークンと有効期限を設定した資格情報を新しいサービスへ渡す
AS-LOCAL-CREDENTIALSを追加し、内部認証を外さず保存・取得・削除と変更なしのV2署名GETが成功しました。
今回の8ケースは1 PASS・6 PASS_WITH_WORKAROUND・1 FAILで、終了コード1です。
既定の`universe_domain`のままの`storage.yml`は未対応です。`universe_domain`を設定する方法は下の[storage.ymlだけでエミュレーターを使う](#storageymlだけでエミュレーターを使う)を参照してください。
AS-LOCAL-CREDENTIALSの方式（有効期限1日のトークン）は、期限後にGoogleへ更新に行くため、このレシピに置き換えました。
googleauthとexternal_accountの制限はSPEC-ACTIVE-STORAGE-LOCAL-CREDENTIALSに記録しています。
既定認証の失敗原因は、SDKのendpoint指定がOAuthの端点を変えないことです。
Googleへの通信は実行していません。
署名付きGETと直接PUTはRails既定のV2署名を送り、URLを変更せず成功します。
回避策つきの6件は、台帳では既定の受け入れ結果をPENDINGのままにしています。
直接アップロード後の読み出しは、fixture側の匿名クライアントを使います。
この読み出しも通し実行で到達し、直接アップロードした本文との一致を確認しました。

## storage.ymlだけでエミュレーターを使う

開発・テストではエミュレーター、本番ではGoogle Cloud Storageを使う構成は、`config/storage.yml`の`gcs_emulator`項目1つで作れます。
利用者向けの手順は[Rails Active Storageでgcs-mimicを使う](rails-active-storage.md)にあります。
テストでも同じ項目（`config/storage.yml`）とバケット設定ファイル（`buckets.json`）を使います。

- `universe_domain`を`googleapis.com`以外にすると、googleauth 1.17.6はOAuthを使わず、自分で署名したJWTを毎回Bearerとして送ります（`ServiceAccountCredentials#enable_self_signed_jwt?`）。トークンの取得も更新も起きません。
- サービス側とcredentials側の`universe_domain`は同じにします。サービス側を省くと、すべての呼び出しが`Universe domain is googleapis.com but credentials are in gcs-emulator.invalid`で失敗します。
- 署名用の鍵は、開発・テストのときだけ起動時に生成します。permissiveプロファイルは署名を検証しません。
- strictモードはIDファイルに登録したトークンしか受け付けません。strictのエミュレーターに対する使い捨ての確認で、最初のアップロードが`401 Invalid Credentials`になったため、この項目はpermissive専用です。

`attachments.rb`で、同じ接続ガードの下、内部の認証を変えずに次のケースを確認しました。ルートは`Rack::MockRequest`でプロセス内から呼び出しています。
2026-10-30、Rails 8.1.4、google-cloud-storage 1.62.1、googleauth 1.17.6で、10 PASS・0 FAIL、本番のケースもPASSです。

リポジトリ外に作った、Puma 8.0.2とgoogleauth 1.17.7のRailsアプリでも、2026-10-30にChromeで確認しました。
3000番と4443番が使用中だったためポートは13000番と14443番に変え、それ以外は[Rails Active Storageでgcs-mimicを使う](rails-active-storage.md)のとおりです。

| 構成 | Chromeでの結果 |
| --- | --- |
| Railsとエミュレーターをホストで動かす | リダイレクトでもプロキシ配信（ブラウザからエミュレーターへの通信なし）でも画像が表示される。Active StorageのJavaScriptによるダイレクトアップロードが、CORSの下で別オリジンへ送られ、同じ本文が保存される |
| RailsをComposeで動かし、`endpoint`が`http://gcs:4443/` | リダイレクト先が`http://gcs:…`になり画像が表示されない。ダイレクトアップロードは`Error storing "direct.png". Status: 0`で失敗 |
| RailsをComposeで動かし、`gcs.localhost`の別名と同じポート番号を使う | `http://gcs.localhost:…`へのリダイレクトで画像が表示され、ダイレクトアップロードで同じ本文が保存され、Rails経由のアップロードも動く |

| ケース | 確認した内容 |
| --- | --- |
| AS-EMULATOR-BUCKETS-FILE | `buckets.json`が、RailsガイドのダイレクトアップロードのCORS設定付きで`local-bucket`を作る |
| AS-EMULATOR-ATTACH | `has_one_attached`の保存・取得と、エミュレーターのバケットにオブジェクトがあること |
| AS-EMULATOR-URL | `attachment.url`を変更せずGETし、本文と一致 |
| AS-EMULATOR-REDIRECT-TARGET | 既定の`rails_blob_path`は、設定した`endpoint`へ302でリダイレクトする（Composeのサービス名ではブラウザが辿れない理由） |
| AS-EMULATOR-PROXY | `resolve_model_to_route = :rails_storage_proxy`で、リダイレクトせずRailsが本文を返す |
| AS-EMULATOR-DIRECT-UPLOAD | 直接アップロードのURLとヘッダーでPUTし、`signed_id`で添付して本文と一致 |
| AS-EMULATOR-DIRECT-UPLOAD-CORS | `http://localhost:3000`からのプリフライトは許可され、許可していないオリジンは拒否。`Origin`付きの`PUT`はCORSヘッダー付きで保存される |
| AS-EMULATOR-V4-DIRECT-UPLOAD | `cache_control`を設定すると、RailsはV4で署名し`Cache-Control`を付ける。`buckets.json`の許可ヘッダーの`Cache-Control`でプリフライトが通り、`PUT`で本文とcache controlが保存される |
| AS-EMULATOR-NO-OAUTH-LATER | 起動から400日後（`travel`）でも、エミュレーター以外へ通信せず保存・取得 |
| AS-EMULATOR-PURGE | `purge`でエミュレーターからオブジェクトが消える |
| AS-EMULATOR-PRODUCTION-NO-KEY | 本番の起動では本番用のサービスが選ばれ、`gcs_emulator`項目の鍵は生成されない |

逆の確認で、各ケースが問題を検出できることも確かめました。

- `universe_domain`を外すと、すべてのケースが`oauth2.googleapis.com`への接続で失敗
- サービス側の値だけ外すと、上のuniverse不一致のエラーで失敗
- CORSの設定を外すと、AS-EMULATOR-BUCKETS-FILEとAS-EMULATOR-DIRECT-UPLOAD-CORSが失敗
- 鍵を常に生成すると、AS-EMULATOR-PRODUCTION-NO-KEYが失敗
- CORSの許可ヘッダーに`Cache-Control`がないと、V4のプリフライトが拒否される（Railsガイドの設定だけの場合と同じ）
- 以前の移行ガイドのレシピ（有効期限1日のトークン）は、400日後のケースで`www.googleapis.com`へのトークン更新が起きて失敗。このレシピに置き換えました

ほかの方法は、この版ではOAuthを避けられません。

- googleauthは鍵ファイルの`token_uri`を読みません。上流の[google-auth-library-ruby#251](https://github.com/googleapis/google-auth-library-ruby/pull/251)はマージされずに閉じています。
- google-cloud-storage 1.62.1にはエミュレーター用の設定がありません。`STORAGE_EMULATOR_HOST`に対応する上流の[google-cloud-ruby#34023](https://github.com/googleapis/google-cloud-ruby/pull/34023)は、2026-10-30時点で未マージです。リリースされれば、このレシピを置き換えられる可能性があります。
- 多くのRailsアプリは、[Railsガイド](https://guides.rubyonrails.org/active_storage_overview.html)の生成どおり、開発・テストでDiskサービスを使います。この場合、署名付きURLなどGCS固有の動きは確認できません。

## V2署名

既知のRuby V4署名の問題は、署名計算のホストが`storage.googleapis.com`になる点です。
今回の既定V2署名では、生成URLがローカル端点へそのまま届いています。
V4のホスト問題とは分けて、SPEC-ACTIVE-STORAGE-V2を今回の範囲で解消しました。

型検査、xtaskの39テスト、依存の最新確認、構文と外部接続防止の自己検査は成功しました。
`just check`も1回実行して成功しました。詳細は[英語の契約と証跡](../en/slices/sdk-active-storage-scenario.md)を参照してください。
日次の依存検出には専用Gemfileとロックファイルを追加しました。

公開配信と実際の画像変換は未検証です。ブラウザーのCORSとstrictモードでの失敗は、[storage.ymlだけでエミュレーターを使う](#storageymlだけでエミュレーターを使う)に記録しています。
strictモードでは`storage.yml`の項目が`401`になり、ログに`auth_self_signed_jwt_rejected`の警告が1回出ます。
自己署名JWTの検証（IDファイルの`public_keys`で検証する）は、strictモードでこのレシピを使いたい要望が出るまで見送ります。
V2対応では独立したGo・Rubyのstrict試験とHTTPSの既存SDKスイートが成功しました。
後続の検証結果はV2の契約文書にまとめています。ベンチマークは行っていません。
