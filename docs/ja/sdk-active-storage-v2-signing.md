# V2 RSA署名URL

<!-- markdownlint-configure-file {"MD013": false} -->

対象はDESIGN-040とDESIGN-084です。どちらもPARTIALを維持します。
新しい設定や依存は追加していません。[英語の契約と証跡](../en/slices/sdk-active-storage-v2-signing.md)が正本です。

| ケース | 結果 | 対応範囲 |
| --- | --- | --- |
| V2-DETECT | PASS | GoogleAccessId・Expires・Signatureの欠落・重複、V4との混在をXML 400で拒否 |
| V2-EXPIRY | PASS | Unix秒を検証し、strictでは注入クロックで期限を判定、期限切れはXML 400 ExpiredToken |
| V2-PERMISSIVE | PASS | V4と同様に署名・期限を検証せず、V2の構造とExpiresの数値形式は検証 |
| V2-STRICT | PASS | 登録済みRSA鍵でSHA256署名を検証、V4と同じ署名者・権限判定 |
| V2-GET | PASS | Go・Rubyの変更していないpath-style署名GET、パス・メソッド・ヘッダー改ざんの拒否 |
| V2-PUT | PASS | 署名付きPUT、Content-Type・Content-MD5の一致、既存のストリーミングPUTによる本文MD5検証 |
| AS-SIGNED-URL | PASS_WITH_WORKAROUND | 認証除去の既存回避策の下でblob.urlをそのまま取得 |
| AS-DIRECT-UPLOAD | PASS_WITH_WORKAROUND | 同じ回避策の下でurl_for_direct_uploadとheaders_for_direct_uploadをそのまま使用、保存内容を照合 |
| V2-HMAC | PENDING | GOOG1/HMAC |
| V2-POST | PENDING | POSTポリシー |
| V2-RESUMABLE | PENDING | resumable開始 |
| V2-VIRTUAL-HOST | PENDING | virtual-host形式の正規化差異 |

V2は公式仕様上、Hostと通常のレスポンス上書きqueryを署名に含みません。
パスはパーセントエンコードを含めてそのまま検証し、URLのhost・path・queryを書き換えません。
V2のHEAD・世代選択・virtual-host形式は明示的に拒否します。HMACはRSA鍵登録の案内付きで失敗します。

2026-10-27に型検査、署名単体11件、HTTPSの`just check-sdk`が成功しました。
Go 1.27.3／Storage 1.70.0、Ruby 4.0.9／Storage 1.62.1を使用しています。
GoのV2 signerはHTTPS固定のため、HTTPの実行では理由付きでスキップします。
Rubyはversion省略と明示的な`:v2`の両方を確認しました。
最終の`just check`と`just check-containers`も各1回成功しました。
コンテナはlinux/arm64とlinux/amd64の両方を確認し、既存ガイドのnative署名手順も成功しました。

`just check-active-storage`は0 PASS・6 PASS_WITH_WORKAROUND・1 FAILで終了コード1でした。
残るAS-DEFAULT-AUTHはOAuthの外部接続を通信前にガードが拒否する既知の失敗です。
署名2ケースの成功を確認し、SPEC-ACTIVE-STORAGE-V2は今回の範囲で解消しました。
既定認証の受け入れ結果はPENDINGのままで、別の認証スライスが担当します。
実Googleとの比較、Railsのstrict構成、ブラウザーCORS、画像変換、ベンチマークは未実施です。
