# Cloud Storageエミュレーター 設計方針

## 目標と現在の状態

Rustで実装し、単体バイナリとDockerで利用できるCloud Storageエミュレーターを目指します。fake-gcs-serverの起動・初期データ投入・接続設定を参考に、Googleの公開仕様を基準に互換性を段階的に高めます。

**この一式は設計・機能台帳・実装プロンプトです。サーバー本体は未実装で、性能の実測値もありません。すべての実装状態は未着手です。**

名称は **gcs-mimic** で確定しました。英語説明は “A lightweight Google Cloud Storage emulator in Rust.” です。mimicによって擬態・エミュレーションの意図を伝えます。レジストリへの登録・公開はまだ行っていません。

「全部再現する」は、公開API、認可、データの状態変化、異常系を追跡する長期目標として扱います。Googleの実際の地域冗長化、SLA、課金、ID発行、ML推論基盤まで単体Docker内で同じにすることはできません。ローカルで検証できる契約、模擬する状態変化、外部サービスとの接続を分けます。

## 構成案

| 要素 | 役割 |
| --- | --- |
| CLI・設定 | 待受、永続化先、外部URL、認証モード、同時実行数、初期データ |
| JSON・XML・gRPC | それぞれの入力形式、ヘッダー、エラー、ストリーミング |
| 共通の処理層 | バケット、オブジェクト、世代、条件付き更新、認可、状態遷移 |
| メタデータDB | 索引、権限、アップロードセッション、操作状態、時刻管理 |
| 本文保存 | ディスク上の一時ファイルと世代ごとの本文 |
| 拡張連携 | 通知、分析、キャッシュ、ML結果の模擬、MCP |

初期は1つのアプリケーションクレートに明確なモジュールを置きます。候補はTokio、Axum/Hyper、Serde、Rustls、SQLiteで、gRPC段階でtonic/prostを追加します。これらは設計案であり、Rustと選択した依存ライブラリは実装開始時の最新安定版を確認して採用し、日次の更新検出と検証によって追従します。採用した状態はtoolchainとlockfileに記録します。

デフォルトはディスク保存です。本文を `Vec<u8>` へ全読み込みせず、一定サイズのバッファで受信・検証・保存します。メモリバックエンドは小さなテスト向けの任意機能とし、上限を設けます。オブジェクト一覧も全件をメモリに読み込んでソートせず、DBの索引とページングを使います。

非同期処理でも無制限にタスク・キュー・ワーカーを増やすと省メモリにはなりません。SQLiteの書き込みトランザクションは短く保ち、アップロード中ずっとロックを持たない構成にします。Tokioのブロッキング処理とSQLite WALの制約を前提に設計します。[Tokio](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)、[SQLite WAL](https://www.sqlite.org/wal.html)。

## 永続化と競合

オブジェクト名をそのままOSのパスにしません。日本語、連続した `/`、`%2F`、末尾の `/`、ドットを含むキーも、論理上の名前として保存します。本文ファイルには不透明なIDを使います。

通常の保存は、一時ファイルへストリーミングし、長さとチェックサムを確認した後、本文を確定し、世代条件を検査したDBトランザクションで公開します。ただし、**ファイルのrenameとDBのcommitは一つの原子的操作ではありません。** 途中で停止した際の復旧手順、孤立ファイルの回収、存在しない本文を参照しない保証まで必要です。

`ifGenerationMatch=0` の同時作成が両方成功しないこと、更新前の世代を読む処理が途中で壊れないことも確認します。通常のアップロードは完了前に本文を公開しません。一方、Rapidの追記可能オブジェクトには書き込み途中から読める仕様があるため、別の状態遷移として扱います。[Preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)、[Rapid Bucket](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket)。

時刻は注入可能にします。署名URL、セッション、保持期間、ライフサイクル、ソフトデリート、UBLAの固定化を、実時間の待機なしで試験できるようにします。

## 公開範囲とURL

| 項目 | 再現する内容 |
| --- | --- |
| 公開URLの生成 | 正しくエンコードしたURLを組み立てる。権限は変更しない |
| 匿名公開 | `allUsers` の有効な許可と公開アクセス防止を評価する |
| 認証済み限定 | `allAuthenticatedUsers` は匿名ユーザーを含めない |
| オブジェクト単位の公開 | ACLやdefault ACL、IAMとの関係を評価する |
| UBLA | ACLの無効化と不可逆になる時刻を扱う |
| 公開アクセス防止 | 公開権限を抑制する。有効な署名URLまで一律に禁止しない |
| 署名URL | V4のRSA/HMAC、期限、HTTPメソッド、パス、クエリ、署名対象ヘッダーを検証する |
| POSTポリシー | フォーム条件、サイズ範囲、署名を独立して検証する |
| 再開用URI | URI自体が持つアクセス能力、期限とアップロード状態を扱う |

`http://localhost:4443/bucket/key` のようなURLを作れても、そのオブジェクトが公開されたことにはなりません。`mediaLink` や `selfLink` の存在も同様です。URL生成・署名ヘルパーを追加する場合はエミュレーター独自ツールとし、GoogleのStorage APIに存在するエンドポイントとして扱いません。[公開データ](https://docs.cloud.google.com/storage/docs/access-public-data)、[署名URL](https://docs.cloud.google.com/storage/docs/access-control/signed-urls)。

V4署名にはHostが入るため、署名後に `storage.googleapis.com` を `localhost` へ置換すると、厳密な検証には通りません。最初から実際にアクセスするホストで署名します。Dockerでは次の3つを区別します。

- 待受先：`0.0.0.0:4443`
- Compose内の接続先：`http://gcs-mimic:4443`
- ホストPCのブラウザからの接続先：`http://localhost:4443`

ブラウザ向け公開URLと、SDKが利用する再開アップロードの `Location` は、利用するネットワークが異なる場合があります。単一の外部URLをすべてへ流用せず、明示した対応関係で返します。[署名の正規化](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests)。

`permissive` と `strict` を明示的に分けます。前者は接続テスト用、後者はローカルの認証情報を使ってIAM・ACL・署名の挙動を検証するためのモードです。前者で成功したことを後者の互換性の証拠にしません。

JSON APIとXML APIではCORSの扱いが異なります。JSONは所定の既定動作、XMLはバケット設定を評価します。CORSを許可しても読み取り権限が増えるわけではありません。静的サイトのindex/404設定も、すべてのダウンロードURLへ一律適用しません。[CORS](https://docs.cloud.google.com/storage/docs/cross-origin)、[エンドポイント](https://docs.cloud.google.com/storage/docs/request-endpoints)。

## DockerとSDK

標準ポート4443、HTTP・任意のHTTPS、CLIと環境変数、初期データ投入、永続ボリューム、ヘルスチェック、終了処理を整えます。fake-gcs-serverのCLI互換は、対応を確認したフラグだけ明記します。署名と期限を検証しない同サーバーの挙動は、厳密な互換仕様にしません。[fake-gcs-server README](https://github.com/fsouza/fake-gcs-server/blob/main/README.md)。

Dockerは `linux/amd64` と `linux/arm64` を対象にし、Apple Siliconでも使える構成にします。非root実行、複数段階ビルド、書き込み先の明示、SQLite WALと一時アップロード先を含む永続化を確認します。初期データ入力と永続保存先は分離し、再起動で意図せず上書きしないようにします。

SDK互換はGoとRuby/Rails Active Storageを先行させ、Node.js、Python、Rustを続けます。Java、C++、.NET、PHP、gcloud、FUSEも台帳で追跡します。SDKごとにバージョン、通信方式、接続設定、署名時の設定を記録し、環境変数1個ですべてのSDKが同じ動きをすると仮定しません。

## 性能の判断方法

Rustを使うだけでは低CPU・低メモリは保証されません。全本文の保持、巨大なキュー、一覧の全件走査、過剰なタスク生成を抑え、測定して判断します。

初期目標の案は、通常のディスク保存構成で待機時RSS 64 MiB以下、1 GiBの転送を16本同時実行した際のピークRSS 256 MiB以下、60秒間の待機CPUが1コアの1%未満です。**実測結果ではなく、最初の実装後に検証・調整する基準です。**

0 B、1 KiB、1 MiB、64 MiB、1 GiB、同時数1/8/16、メタデータ1千/10万件を使い、RSS、ページキャッシュを含むコンテナメモリ、CPU秒/GiB、RPS、転送量、p50/p95/p99、エラー率、一時ディスク量を測定します。同じ保存方式、TLS、認可、チェックサム、耐障害性、CPU制限でfake-gcs-serverと比較します。認可や永続化を省いた測定を、同等機能での勝利として扱いません。

## 言語方針

実装プロンプトと会話は日本語、コード、コメント、コミット、Issue、PR、CLIヘルプ、正本ドキュメントは英語です。この日本語資料は当初の希望に対応した補助資料としてプロジェクトの外に分けています。任意の日本語表示も同じJSON台帳を使い、進捗を二重管理しません。
