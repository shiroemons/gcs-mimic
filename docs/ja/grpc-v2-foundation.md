# Storage v2 gRPCの土台

<!-- markdownlint-configure-file {"MD013": false} -->

[英語の契約と検証](../en/slices/grpc-v2-foundation.md)が正本です。
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
HTTP・HTTPSと接続上限32本を共有し、接続ごとに最大8ストリーム、メッセージは最大4MiBです。
SIGTERMでは同じ受付停止・drain・カタログcloseの順で終了します。
strictは共有AuthでBearerを判定し、ローカルprincipalを各RPCグループへ渡します。
共有受け口はトークンなしを匿名Callerとして渡し、RPC側の共通関数が現時点では
`UNAUTHENTICATED`にします。不正なトークンは受け口で拒否し、permissiveは素通しです。
`connection_timeout_ms`は開いているストリームがない接続の無通信期限です。
`grpc_idle_connection_closes_with_goaway`では、期限切れにGOAWAYを送ってから閉じ、HTTPと共有する接続枠を解放します。
`grpc_idle_never_cuts_in_flight_streams`では、リクエスト開始からレスポンス本文の終了・dropまで数を保持し、
応答待ちや本文のフレーム間隔が期限を超えてもアイドル扱いしません。減算はDropで行います。
`grpc_dead_peer_is_detected_by_keepalive`では、開いたストリームの相手が応答しなくなった場合、
間隔・応答期限とも`connection_timeout_ms`のHTTP/2 keepaliveで検出して閉じます。
`grpc_goaway_has_bounded_grace`では、GOAWAY後の猶予を、アイドル時は
`connection_timeout_ms`、SIGTERM時は
`min(connection_timeout_ms, shutdown_timeout_ms / 2)`に制限します。
全体の停止予算の半分を接続の後始末・joinに残し、両方が既定の5000msでも正常終了させるためです。
すでにアイドルのGOAWAY中でもSIGTERMで猶予を短縮し、共有の停止順序は変えません。
未完のhandshakeにはGOAWAYを送れない場合があり、猶予後に強制終了します。
アプリ側のタイマーは接続ごとに1本を共用し、ストリーム数の変更はwatch通知で受け取ります。
アイドル切断はDebug、keepalive失敗・猶予切れは原因と設定値の例を含むWarnで記録します。

protoと生成コードはコミット済みなので、通常ビルドにprotocは不要です。
resource nameの解析と、共有記録からのBucket/Object変換はRustの単体テストで確認します。
各RPCの実処理、Storage v1、RapidのappendとCancelResumableWriteは後続の範囲です。
ベンチマーク、コンテナ、ガイドの通し実行と実クラウド試験は行いません。

2026-10-27の接続管理の追加検証では、RustのgRPC単体8件、実プロセス2件、
GoのgRPC foundation全件がPASSしました。既定の接続・停止期限がともに5000msでも、
prefaceを送らない接続を保持したままSIGTERMで正常終了し、カタログを再openできます。
最終ソースの`RUST_TEST_THREADS=8 just check`もPASSしました。
台帳の4ケースをPASSに更新し、BEHAVIOR-RPC-005はPARTIALを維持しています。
機能表は英日とも再生成し、状態・fidelity・SDK証跡数が同じため内容の差分はありません。
HTTPは変更していないため、HTTP・Ruby SDKの全体実行は繰り返していません。
