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

HTTPの隔離サーバーを起動して7ケースを個別に記録し、失敗しても残りを実行します。
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

2026-10-27の通し実行の結果は次のとおりです。

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
既定認証の失敗原因は、SDKのendpoint指定がOAuthの端点を変えないことです。
次の候補は`sdk-active-storage-local-auth`で、利用者が指定できるローカル認証設定か、
別途範囲を決めたトークン用端点を検証します。Googleへの通信は実行していません。
署名付きGETと直接PUTはRails既定のV2署名を送り、URLを変更せず成功します。
回避策つきの6件は、台帳では既定の受け入れ結果をPENDINGのままにしています。
直接アップロード後の読み出しは、fixture側の匿名クライアントを使います。
この読み出しも通し実行で到達し、直接アップロードした本文との一致を確認しました。

既知のRuby V4署名の問題は、署名計算のホストが`storage.googleapis.com`になる点です。
今回の既定V2署名では、生成URLがローカル端点へそのまま届いています。
V4のホスト問題とは分けて、SPEC-ACTIVE-STORAGE-V2を今回の範囲で解消しました。

型検査、xtaskの39テスト、依存の最新確認、構文と外部接続防止の自己検査は成功しました。
`just check`も1回実行して成功しました。詳細は[英語の契約と証跡](../en/slices/sdk-active-storage-scenario.md)を参照してください。
日次の依存検出には専用Gemfileとロックファイルを追加しました。

Railsのstrict構成、ブラウザーのCORS、公開配信、実際の画像変換は未検証です。
V2対応では独立したGo・Rubyのstrict試験とHTTPSの既存SDKスイートが成功しました。
後続の検証結果はV2の契約文書にまとめています。ベンチマークは行っていません。
