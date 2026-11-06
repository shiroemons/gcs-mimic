# Storage v2 gRPCの土台

<!-- markdownlint-configure-file {"MD013": false} -->

この文書は土台スライスを追加した時点の記録です。以下の全RPCの拒否は当時の検証を指します。
現在のStorage v2の部分対応は、[バケット](grpc-v2-buckets.md)、
[オブジェクト入出力](grpc-v2-object-io.md)、[更新・派生操作](grpc-v2-update-copy.md)、
[双方向範囲読み取り](grpc-v2-bidi-read.md)の契約を参照してください。
CancelResumableWriteはオブジェクト入出力の契約に含まれます。appendableオブジェクトは[Rapid appendの契約](grpc-v2-rapid-append.md)を参照してください。未対応フィールドはUNIMPLEMENTEDのままです。
Storage v1の移行プロファイルは[その契約](grpc-v1-legacy.md)を参照してください。

[英語の契約と検証](../en/slices/grpc-v2-foundation.md)が正本です。

ルーティングメタデータの変更を統合する前に、既存のGo・RustのgRPCハーネスを実行します。

```sh
CARGO_BUILD_JOBS=4 env -u GCS_MIMIC_GO_TEST_RUN GCS_MIMIC_SDK_SCHEME=http \
  bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-rust
```

起動スクリプトがGo用のpermissive・strictエンドポイントとRust用のGCS_MIMIC_GRPC_ENDPOINTを設定します。
追加のRustスイートを動かすため、Goのフィルターは解除します。
これはRustワークスペースのテストとは別のSDKゲートであり、文書更新だけでは実行済みになりません。
BEHAVIOR-RPC-005とDESIGN-082はPARTIALのままです。
RPC-STORAGE-001〜024の状態は変えず、受け口の追加だけをnotesに記録します。

`grpc_listen`の既定値は未設定、つまり無効です。GoのgRPCクライアントにはHTTP・HTTPSと
別の平文ポートが必要なため、`--grpc-listen 127.0.0.1:4444`、環境変数
`GCS_MIMIC_GRPC_LISTEN=127.0.0.1:4444`、または設定ファイルの
`{"grpc_listen":"127.0.0.1:4444"}`で有効にします。CLI、環境変数、ファイルの順で優先します。
HTTPの公開URLやAPI URLは変えません。readyイベントのlistenersにh2cの実アドレスを加えます。

Go SDKは`STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444`と
`storage.NewGRPCClient(ctx)`で接続します。`NewMultiRangeDownloader`にはSDKの制約で
`storage.WithGRPCBidiReads()`も必要です。Bucket.Attrs、WriterのCloseまで、multi-rangeの
呼び出しは期限前に失敗し、`storage.ShouldRetry(err)`はfalseになります。
全24RPCと未知のサービス・メソッドは`UNIMPLEMENTED`を返します。
`google.rpc.Status`のErrorInfoはreasonを`badRequest`、domainを`global`とし、
gcs-mimicの未対応とJSON APIへの切り替え例を示します。not foundとは区別できます。

起動は既存の復旧・初期化・seedの後にbindし、ready出力後に受け付けます。
HTTP・HTTPSと接続上限1024本を共有し、接続ごとのストリームは`max_grpc_streams`（既定100、1〜65536、`grpc_listen`が必須）まで、受信メッセージは最大4MiB、送信メッセージはStorage v2が最大33MiB、Storage v1が最大4MiBです。
v2の上限は最大のバケットリソースに合わせています。CORSとライフサイクルのルールは、別々の書き込みでそれぞれJSON本文の上限16MiBまで届きます。受信上限が4MiBのままのクライアントでは、そのようなリソースの読み取りはクライアント側で失敗するはずで、`read_mask`で除けば読めます。実際のSDKでは確認していません。
既定の100はRFC 9113 6.5.2節の推奨値です。ストリームごとに最大4MiBの要求メッセージとエンコードした応答をバッファするので、1接続で要求だけでも最大400MiBになりえます。メモリーを抑えるには下げてください。
接続枠が足りなくなると、最も長くアイドルなgRPC接続はGOAWAYを送り、HTTP/1と同じ行列で枠を譲ります（`grpc_idle_reclaimed`、debug）。
新しい接続は最初のストリームが終わるか、最大5秒の猶予が過ぎるまで行列に入りません。GOAWAYと競合して開かれたストリームは切りません。この場合、待っているクライアントは他の接続が終わるのを待ちます。上限は他の接続のkeep-aliveで、失敗はしません。
沈黙する相手が枠を持つのは`header_read_timeout_ms`と5秒の短い方までです。
SIGTERMでは同じ受付停止・drain・カタログcloseの順で終了します。
strictは共有AuthでBearerを判定し、ローカルprincipalを各RPCグループへ渡します。
共有受け口はトークンなしを匿名Callerとして渡し、RPC側の共通関数が現時点では
`UNAUTHENTICATED`にします。不正なトークンは受け口で拒否し、permissiveは素通しです。
接続の段階ごとに別の設定を使います。HTTP/2のprefaceが届くまでが`header_read_timeout_ms`（既定30000）、
開いているストリームがない接続の無通信が`keep_alive_timeout_ms`（既定120000）です。
`grpc_idle_connection_closes_with_goaway`では、期限切れにGOAWAYを送ってから閉じ、HTTPと共有する接続枠を解放します。
`grpc_idle_never_cuts_in_flight_streams`では、リクエスト開始からレスポンス本文の終了・dropまで数を保持し、
応答待ちや本文のフレーム間隔が期限を超えてもアイドル扱いしません。減算はDropで行います。
ストリームを開いたままの無通信には`body_inactivity_timeout_ms`（既定0で無効）を使います。
`grpc_silent_open_stream_is_bounded`では、これを指定した場合に、
リクエスト・レスポンスのHEADERS、DATA、trailersがその間出入りしなければGOAWAYを送り、
猶予後に閉じてHTTPと共有する接続枠を解放します。PING・PONGは活動に数えません。
以前の固定60秒は、GoのMultiRangeDownloaderが次の範囲を待つ正当な使い方を切る場合があり、設定で変えられないので廃止しました。
`grpc_active_stream_survives_past_bound`では、この上限より短い間隔で送信または受信が続く限り、
合計時間が上限を超えても切断しません。接続ごとに最後のフレーム時刻を1つ保持します。
`grpc_dead_peer_is_detected_by_keepalive`では、開いたストリームの相手が応答しなくなった場合、
間隔・応答期限とも`header_read_timeout_ms`のHTTP/2 keepaliveで検出して閉じます。
これを0にするとpingを止め、相手の消失はTCP keep-aliveに任せます。プローブは4回で、アイドルな接続の相手の消失は90秒で検出します。
`grpc_goaway_has_bounded_grace`では、GOAWAY後の猶予を、アイドル時は
`header_read_timeout_ms`と5秒の短い方（0なら5秒）、SIGTERM時は`header_read_timeout_ms`（0なら5秒）と`shutdown_timeout_ms / 2`の小さい方に制限します。
全体の停止予算の半分を接続の後始末・joinに残し、正常終了させるためです。`shutdown_timeout_ms`が0なら予算の制限はありません。
すでにアイドルのGOAWAY中でもSIGTERMで猶予を短縮し、共有の停止順序は変えません。
未完のhandshakeにはGOAWAYを送れない場合があり、猶予後に強制終了します。prefaceが届かないまま期限が切れると`grpc_handshake_timeout`を記録します。
アプリ側のタイマーは接続ごとに1本を共用し、ストリーム数と送受信の変更はwatch通知で受け取ります。
テストでは同じ接続処理へ短い上限を渡します。無通信上限による切断はInfoで原因と、定期送信またはストリームの開き直しという対処を記録します。
アイドル切断はDebug、keepalive失敗・猶予切れは原因と設定値の例を含むWarnで記録します。

protoと生成コードはコミット済みなので、通常ビルドにprotocは不要です。
resource nameの解析と、共有記録からのBucket/Object変換はRustの単体テストで確認します。
各RPCの実処理、Storage v1とRapid appendは後続のスライスで実装しました。
ベンチマーク、コンテナ、ガイドの通し実行と実クラウド試験は行いません。

2026-10-27の接続管理の追加検証では、RustのgRPC単体8件、実プロセス2件、
GoのgRPC foundation全件がPASSしました。既定の接続・停止期限がともに5000msでも、
prefaceを送らない接続を保持したままSIGTERMで正常終了し、カタログを再openできます。
最終ソースの`RUST_TEST_THREADS=8 just check`もPASSしました。
台帳の4ケースをPASSに更新し、BEHAVIOR-RPC-005はPARTIALを維持しています。
機能表は英日とも再生成し、状態・fidelity・SDK証跡数が同じため内容の差分はありません。
HTTPは変更していないため、HTTP・Ruby SDKの全体実行は繰り返していません。

無通信上限の追加では、受け入れ2ケースを実装前に契約と台帳へ記録しました。
2026-10-27の最終接続ソースでRustのgRPC単体10件、Go foundation全件とGo vetがPASSしました。
短い上限を注入し、応答待ち・開いたレスポンス本文でPINGに応えてもGOAWAY後に閉じることと、
リクエスト側・レスポンス側それぞれの送受信で上限の2倍を超えて継続することを確認しました。
`RUST_TEST_THREADS=8 just check`も、既存の実プロセス2件を含めてPASSしました。
台帳の追加2ケースをPASSにし、BEHAVIOR-RPC-005はPARTIALを維持しています。
英日機能表は再生成して差分なしです。上限は接続単位なので、同じ接続内の別ストリームの通信でも更新します。
HTTP・Ruby SDK全体、コンテナ、ガイド、ベンチマーク、実クラウド試験は対象外として実行していません。
