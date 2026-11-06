# 稼働中の同期をCLIで操作する

<!-- markdownlint-configure-file {"MD013": false} -->

[English](../en/slices/sync-cli.md) · [機能表](feature-matrix.md)

0.8.0で追加した`sync`コマンドは、稼働中サーバーの管理APIへ接続します。
別プロセスからdata_dirを開かず、サーバーの設定や権限を変更しません。
DESIGN-072・DESIGN-074全体はPARTIALです。0.7.0以前にはこのCLIはありません。

```sh
gcs-mimic sync status --endpoint https://127.0.0.1:4443
gcs-mimic sync status --view findings --endpoint https://127.0.0.1:4443 --revision FINDINGS_REVISION
gcs-mimic sync conflicts --endpoint https://127.0.0.1:4443 --limit 256
gcs-mimic sync status --view deletions --endpoint https://127.0.0.1:4443 --revision DELETION_REVISION
gcs-mimic sync rescan --endpoint https://127.0.0.1:4443
gcs-mimic sync approve --endpoint https://127.0.0.1:4443 --deletion-set DELETION_REVISION
gcs-mimic sync wait --endpoint https://127.0.0.1:4443 --timeout 10s
gcs-mimic sync resolve --endpoint https://127.0.0.1:4443 --conflict CONFLICT_ID --revision TARGET_REVISION --action restore --expected-generation 123 --operation-id restore-one
```

大文字の値はプレースホルダーです。statusの対応するrevisionをそのまま指定します。
findingsは48文字、削除一覧と承認は32文字の小文字16進数です。
削除対象の全ページを確認してから、同じrevisionを承認してください。
`--cursor`は返された値を変更せず渡し、`--limit`には1～256を指定します。
CLIは1ページだけ表示し、古くなったrevisionを自動で取得し直して承認することはありません。

競合一覧の初回はrevisionを省略します。続きは返された32文字の一覧revisionとcursorを使います。
各項目の対象revisionは、一覧revisionとは別です。resolveは対象・操作・operation IDの明示指定を
サーバーのJSONへ変換します。明示的なrestoreはAPI更新のjournalを通り、
復元したbytesを作業ツリーへ反映します。通常の外部取り込みのecho抑制は維持します。

競合の解決ではconflict_id、対象revision、actionのrestoreかdiscard、expected_generation、
operation_idを指定します。generationの0は対象なしを表します。
復旧データではrecovery_id、revision、target_identity、actionのdiscard、operation_idと、
confirmationのbackup_verifiedかdiscard_without_backupを指定し、2種類の対象を混ぜません。
CLIでは競合に`--conflict ID --revision REV --action restore|discard --expected-generation N --operation-id ID`を指定します。
復旧では`--recovery ID --revision REV --target-identity ID --action discard --operation-id ID`に加え、
`--confirm-backup-verified ID`か`--confirm-discard-without-backup ID`を1つだけ指定します。
確認IDはrecovery IDと一致する必要があります。対象の混在、generationや確認の不足、不一致は送信前に拒否します。
discardの前に必要なbackupを保持してください。同じoperation_idで異なる入力を再送すると失敗します。
応答が途切れても、受け付け済みの操作が取り消されたとは限りません。

`--endpoint`はポート付きのHTTP(S) originです。パス・query・認証情報を含めません。
名前解決した全アドレスがloopback、RFC1918 IPv4、IPv6 unique-localのいずれかである必要があります。
最大8件までを検査し、その数値アドレスへ接続します。外部公開アドレス、metadata/link-local、
未指定・multicastは拒否します。proxyやGoogle認証情報の自動探索、redirectは使いません。
コンテナ間通信用にprivate networkのHTTPも使えます。loopback以外では、tokenを平文で流さないようHTTPSを使ってください。

HTTPSは既定の開発用CAとhostnameを検証します。`--ca-cert PATH`でCAを明示的に置き換えられます。
HTTPにこのオプションを指定すると失敗し、TLS検証を省くオプションはありません。

認証はCLI専用の`GCS_MIMIC_SYNC_TOKEN`か、優先される`--token-file PATH`を使います。
ファイルには末尾の改行を除いて1つのbearer tokenを書き、上限は8192 bytesです。
CLI専用変数をサーバーの環境変数に混ぜません。サーバーは現在の同期操作権限を検査し直し、
通常のバケットIAMやfixture管理者の権限で代用することはできません。

通信期限の既定値はrescanが30000 ms、それ以外が10000 msです。rescanでは静穏待ちの上限10秒と、その後の走査を見込みます。
`--timeout-ms`に1～60000、または`--timeout`に整数とms・sの接尾辞を指定すると、既定値を置き換えます。
waitではその値をサーバーの観測期限として送り、応答を受け取るためにCLI側で1秒を追加します。
認可のDB待ちと本文の準備にはcatalogの待機期限を使い、connectionのinactivityとして扱いません。
認可済みの本文を読めた後に指定の観測期限へ切り替え、エラーや取消しでも待機保護を解放します。
観測するscan・journal fenceを固定するため、後から続く無関係な書き込みを待ち続けません。
期限はsettle待機と、scan・journal再生・削除の処理間で確認します。開始済みのI/Oは途中で取り消しません。
未完了のscanを完了扱いにしたり、その結果で削除を承認したりはしません。
statusのwritebackから、常にnullだった`journal_sequence`と`journal_epoch`を除きました。
実際に待つjournal fenceは、wait応答の`journal_fence`で確認できます。
削除候補を再計算している間、statusは最後に確定した削除候補を返し、安全上のfindingとrevisionは現在の状態に更新します。
権限とjournal fenceは読み取りごとに再確認し、古いrevisionでのページ取得や承認は409になります。
native通知の後は1秒の静穏を待ち、追加通知で待ち直します。上限は10秒です。
静穏待ちのwaitとrescanはアクター内の有界キューで待ち、CLI側で要求を再試行する必要はありません。
waitは元の指定期限を保ち、その期限でも静穏が成立していない場合に初めてSourceChangingで409を返します。
rescanは静穏成立か10秒の上限後に実際のscanを行い、実行前に成功扱いにはしません。
上限でscanを行ってもwaitは完了せず、元の期限を保って次の静穏を待ちます。
待機中の要求は観測処理の停止・回復をまたいで保持します。
静穏待ちの間も承認・解決を受け付けますが、有界キューが満杯の場合はbusyを返します。
自動削除数はschema 50で永続化し、削除済み件数と今回の候補を合わせて保護の閾値を判定します。
承認待ちのrescanや再起動ではリセットせず、承認・新しいbaselineの確定・静穏成立後に完了した候補0件のscanでリセットします。
自動scanと明示scanは同じ条件です。静穏上限での打ち切り、不完全なscan、候補が残るscanではリセットしません。
scanの前後でnative通知を確認し、イベント世代が変わらず未処理の通知がないことも条件にします。
リセットに失敗した場合は履歴を保持して原因をログに記録し、成功したwait・rescanの応答を返します。
静穏上限より長い操作では、累積が閾値に届く前の少数の自動削除が確定する場合があります。
schema 49から50への移行は一方向です。更新前に停止したデータディレクトリ全体をbackupしてください。
移行時の累積数は0から始まり、旧スキーマで既に削除した名前は復元できません。
期限切れ408、停止中409を含む非成功応答は、JSONを表示して非0で終了します。
timeoutや切断の後も、受け付け済みの走査・公開処理が完了する場合があります。
mutationを自動再送しないため、再試行前にstatusを確認してください。

応答はheader 64件・16 KiB、JSON本文256 KiBまでです。chunked応答にも同じ上限を適用し、
壊れたJSON、途中で切れた本文、超過を成功扱いにしません。
JSON以外の本文を受け取った場合は、本文を表示せずHTTP statusを含むエラーを返します。
競合解決では、サーバーが対象revision・identity・権限・明示確認を検査します。

実CLIと実サーバーを使う受入は、既存4件と追加5件で次を確認します。

- 100件の外部削除を検出し、同じrevision・分母・journal fenceの全3ページを確認して承認し、wait後のAPI削除を確認します。古い・誤った・再利用した承認は409です。
- 2ページの競合を確認し、binary bytesをrestore、空の競合をdiscardします。古いgenerationやhold中のrestoreはAPIと保持データを変更しません。成功後は作業ツリーにも反映され、同じoperation IDの再送と再起動でgenerationを維持します。異なる入力で同じIDを使うと失敗します。
- strictでは、最後のバケットを削除して別projectで同名を再作成しても、保持された元ownerのproject管理権限が必要です。現在のownerだけでは表示・wait・解決は403となり、全権限があっても古いincarnationへのrestoreは409です。
- 1 TiB＋1 byteの疎ファイルを持つ復旧停止を検査し、再起動後も長さとbinary先頭を保持します。古いrevision、異なる対象・identity、元owner権限不足、確認不足はデータを残します。対象を特定した明示discard後だけ停止を解消し、rescan・wait・再起動でもAPIのmetadataとbytesを維持します。
- 実サーバーでrestoreがcommitした後、中継が応答を落としてもCLIは自動再送しません。明示的に同じ操作を再送すると、同じgenerationを返します。

登録済み競合は既存の公開catalog APIと同期処理で準備します。疎ファイルの復旧だけは、
停止済みテストcatalogに既存schemaのArmed intentを作り、実サーバーの通常の検査・保護・quota・認可を通します。
これは停止状態からの操作を確かめるfixtureで、原因となるI/O障害の発生を再現した証拠ではありません。
疎ファイルは長さと先頭だけを確認し、全体を読みません。backup確認は操作者の申告で、CLIがbackupを検証するわけではありません。

既存の[障害注入・PowerLossモデル（英語）](../en/slices/sync-conflict-recovery.md)と
[書戻し復旧（英語）](../en/slices/native-writeback-recovery.md)は別の証拠です。
物理電断や全命令境界を保証しません。サーバー自身が返す503、Linux/Windowsの全面的な動作、
watcher・alias・overflow・bind mount、終了や障害の全境界、Google実環境はこのCLI受入の対象外です。
親DESIGN-072・DESIGN-074はPARTIALのままです。SDK・コンテナ・benchmarkは本作業では実行しません。
コマンド・test ID・検証結果は[CLIの記録（英語）](../en/slices/sync-cli.md#native-operator-acceptance)、
全体の検証は[統合記録（英語）](../en/evidence/migration-integration.md)を参照してください。

2026-10-31にmacOS ARM64・Rust 1.99.1で型検査を先に実行し、実CLI 9件、parser/mock 10件、
conflict storage 22件、書戻し復旧12件がすべて成功しました。失敗・skipはありません。
production lintは違反0件で、CLI/binaryのClippy、整形・空白・日英Markdownの検査も通過しています。
100件fixtureの初回走査だけはCLIの期限を明示的に60秒にし、その後のwaitは10秒です。
CLIの既定値10秒は変更しておらず、走査の所要時間を保証する結果でもありません。
