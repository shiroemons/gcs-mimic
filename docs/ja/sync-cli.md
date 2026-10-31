# 稼働中の同期をCLIで操作する

<!-- markdownlint-configure-file {"MD013": false} -->

[English](../en/slices/sync-cli.md) · [機能表](feature-matrix.md)

開発中ソースの`sync`コマンドは、稼働中サーバーの管理APIへ接続します。
別プロセスからdata_dirを開かず、サーバーの設定や権限を変更しません。
DESIGN-072・DESIGN-074全体はPARTIALです。公開済み0.7.0にはこのCLIはありません。

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

HTTPSは既定の開発用CAとhostnameを検証します。`--ca-cert PATH`でCAを明示的に置き換えられます。
HTTPにこのオプションを指定すると失敗し、TLS検証を省くオプションはありません。

認証はCLI専用の`GCS_MIMIC_SYNC_TOKEN`か、優先される`--token-file PATH`を使います。
ファイルには末尾の改行を除いて1つのbearer tokenを書き、上限は8192 bytesです。
CLI専用変数をサーバーの環境変数に混ぜません。サーバーは現在の同期操作権限を検査し直し、
通常のバケットIAMやfixture管理者の権限で代用することはできません。

通信期限の既定値は10000 msで、`--timeout-ms`に1～60000、または`--timeout`に整数とms・sの接尾辞を指定できます。
waitではその値をサーバーの観測期限として送り、応答を受け取るためにCLI側で1秒を追加します。
観測するscan・journal fenceを固定するため、後から続く無関係な書き込みを待ち続けません。
期限切れ408、停止中409を含む非成功応答は、JSONを表示して非0で終了します。
timeoutや切断の後も、受け付け済みの走査・公開処理が完了する場合があります。
mutationを自動再送しないため、再試行前にstatusを確認してください。

応答はheader 64件・16 KiB、JSON本文256 KiBまでです。chunked応答にも同じ上限を適用し、
壊れたJSON、途中で切れた本文、超過を成功扱いにしません。
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
物理電断や全命令境界を保証しません。サーバー自身の混雑時503、Linux/Windowsの全面的な動作、
watcher・alias・overflow・bind mount、終了や障害の全境界、Google実環境はこのCLI受入の対象外です。
親DESIGN-072・DESIGN-074はPARTIALのままです。SDK・コンテナ・benchmarkは本作業では実行しません。
コマンド・test ID・検証結果は[CLIの記録（英語）](../en/slices/sync-cli.md#native-operator-acceptance)、
全体の検証は[統合記録（英語）](../en/evidence/migration-integration.md)を参照してください。

2026-10-31にmacOS ARM64・Rust 1.99.1で型検査を先に実行し、実CLI 9件、parser/mock 10件、
conflict storage 22件、書戻し復旧12件がすべて成功しました。失敗・skipはありません。
production lintは違反0件で、CLI/binaryのClippy、整形・空白・日英Markdownの検査も通過しています。
100件fixtureの初回走査だけはCLIの期限を明示的に60秒にし、その後のwaitは10秒です。
CLIの既定値10秒は変更しておらず、走査の所要時間を保証する結果でもありません。
