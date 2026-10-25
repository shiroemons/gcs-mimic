# 起動フェーズと中断

<!-- markdownlint-configure-file {"MD013": false} -->

対象はDESIGN-062、DESIGN-072、DESIGN-074です。各機能の広い範囲はPARTIALのままです。UX-002の初期ファイル読み込み後にreadyを出す範囲はIMPLEMENTEDを維持します。[英語の契約](../en/slices/startup-phases.md)と[共有契約](../en/slices/onboarding-state-contract.md)を正本とします。

設定・TLS・IDレジストリを検証した後、カタログのopenと復旧、PAP、バケット初期化、seed、bind、保守処理の開始、readyの順に進みます。ready JSONの既存フィールドに`buckets_file`の適用結果を加えます。Importの実行中マーカーとディレクトリ同期の統合は後続スライスの範囲です。

実行環境のシグナル登録後は、非同期のbind前処理全体をshutdownと競わせます。復旧中に停止要求を受けた場合は、既に開始した復旧が完了してからカタログを閉じます。PAP・seed中は新しい要求を止め、既に開始した書き込みと片付けが終わるまで待ちます。PAP・bindの失敗でも明示的にcloseします。停止要求ではロックを解放してからstoppedを出し、失敗時はreadyを出さず非ゼロで終了します。

中断できる待機をCancelSafe型で明示し、PAP・初期化・seed・bindを個別にshutdownと競わせます。これはレビュー用の目印で、コンパイラが中断安全性を証明する型ではありません。初期化の読込・検証ジョブはカタログを持たず、中断後に書き込めません。投入済みの管理用SQLはカタログが所有し、closeで完了を待ちます。検証中の本番起動経路を中断するテストも追加しています。同期処理の統合は後続です。ReadyEvent・SeedOutcome・InitOutcomeを型で定義し、既存フィールドを維持して初期化結果を追加します。

seed中のSIGTERM、復旧・PAP中の中断、PAP・bind失敗後の解放、ready前のプローブ失敗、再起動後のデータ保持をテストしています。64 MiBの本文を使い、stagingへの転送中とrename後・commit前にも停止させます。停止後はstagingが空で、blobファイルと確定済みの参照が一致し、再起動で収束することを確認します。起動時間は[測定証跡](../en/evidence/startup-phases.md)を参照してください。

非同期の起動中にshutdownを受け取ると、stderrにstoppingを出してから処理をdrainします。テストはこの受信確認後にロック保持を確かめます。起動前後のclose失敗は共通の診断合成を使い、元の失敗とcloseの種類・原因を残します。

復旧中のshutdownは復旧が完了するまで上限なく待ち、2回目のシグナルによる短縮もしません。起動時間の実測値はshutdown待ち時間の上限ではありません。復旧処理の中身、公開済み本文の保護方式、スキーマは変更していません。設定・TLS・IDレジストリの検証は従来どおりシグナル登録前の同期処理です。このスライスのWindows/Linux実行、コンテナ・SDK通し確認、電源断、cgroupメモリ、cold cacheは未検証です。
