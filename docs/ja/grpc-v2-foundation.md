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
`connection_timeout_ms`はgRPCの無通信接続にも適用し、本文の送受信で期限を更新します。
期限は停止中も有効なので、未完のhandshakeが共有枠やdrainを占有し続けません。

protoと生成コードはコミット済みなので、通常ビルドにprotocは不要です。
resource nameの解析と、共有記録からのBucket/Object変換はRustの単体テストで確認します。
各RPCの実処理、Storage v1、RapidのappendとCancelResumableWriteは後続の範囲です。
ベンチマーク、コンテナ、ガイドの通し実行と実クラウド試験は行いません。
