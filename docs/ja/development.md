# 開発とpush前の確認

[English](../en/development.md)

開発中は`just check`、push前の最終ソースでは`just pre-push`を実行します。
コンテナのビルドと動作確認は、AMD64とARM64を並列に進めます。

## 準備

macOSまたはLinuxで、Bash、just 1.56.0以降、rustup、Go、jq、curl、
Node.js 24以降（npm付き）、uv 0.13.6以降を使います。
Ruby・Bundler・gemは[SDKテストの指定バージョン](../../sdk-tests/ruby/README.md)を
揃えてください。PythonはuvがPython SDKテスト用の指定バージョンを用意するので、
別途インストールする必要はありません。PHP SDKテストはDocker上で動くので、PHPも不要です。

Docker、Compose、Buildxも必要です。Dockerを起動し、AMD64とARM64の
Linuxコンテナを実行できるようにします。ホストと異なる環境ではエミュレーションを使います。
依存の取得やイメージのpullには、ネットワーク接続が必要になる場合があります。

## 実行する

```sh
just check
just pre-push
```

`just pre-push`は次の順序で進みます。

1. Rustとシェルの検証。
2. Go・Ruby・Node.js・Python・PHP SDKの全テスト。
3. AMD64・ARM64のコンテナ検証を並列実行。

各環境は、自分のビルドが終わり次第、動作確認へ進みます。
ログには`[linux/amd64]`または`[linux/arm64]`が付きます。
イメージ名、コンテナ、ボリューム、ホスト側のポートは環境ごとに分離されます。

実行中は`[pre-push N/3 P%]`の行で進み具合を表示します。各段階の開始時と終了時、
および実行中は30秒ごとに、経過時間と残り時間の目安を出します。目安は前回の実行で
記録した各段階の所要時間（`target/pre-push-progress/`）から計算します。初回や
`cargo clean`の後は段階の数だけで進捗率を出します。表示の間隔は
`GCSM_PRE_PUSH_PROGRESS_INTERVAL`（秒）で変えられ、`0`にすると段階の切り替わりだけを表示します。

失敗した段階から次の段階には進みません。コンテナ検証では両方の終了と後片付けを待ち、
片方でも失敗するとコマンド全体が失敗します。エラーを修正し、最終ソースで再実行してください。
このコマンド自体はコミットやpushを行いません。

## テストの実行時間

`scripts/test-rust.sh`は、`cargo test --workspace`と同じテストバイナリとdoctestをすべて実行します。
テストバイナリは`GCSM_TEST_JOBS`本ずつ（既定は1）並列に実行し、
doctestもその間に進めます。SDK用サーバーは最初にビルドします。
このビルドとテストのビルドはどちらも、統合テストが起動する`target/debug/gcs-mimic`を置き換えるため、
実行中のテストと重ねられません。

`just check`と`just check-sdk`は`scripts/with-test-tmpdir.sh`を通して実行します。
macOSでは実行中だけRAMディスク（`GCSM_TEST_RAMDISK_MB`、既定は4096）を作って`TMPDIR`にし、
並列数の既定を4に上げ、終了時に取り外します。テストのファイル同期はそのまま行います。
macOSの同期は`F_FULLFSYNC`でシステムディスク全体を書き出すため、通常はこれがテスト時間の大半を占めます。
システムディスク上で並列に実行すると、期限を確かめるテストが遅れて失敗するため、その場合は逐次実行のままにします。
通常の`TMPDIR`を使うには`GCSM_TEST_RAMDISK=0`を指定します。
RAMディスクを作れなかった場合は警告を出し、逐次実行で続けます。
macOS以外では何も変えず、CIも逐次実行です。
そのため、手元のmacOSとCIではテストの時間条件が異なり、時間に敏感な失敗が片方だけに出ることがあります。
どちらで出た場合もテストの欠陥として扱ってください。
これらの設定に`GCSM_TEST_`接頭辞を使うのは、サーバーが`GCS_MIMIC_`と`FAKE_GCS_`で始まる変数を
すべて自分の設定として読み、知らない名前を拒否するためです。

## コンテナ検証だけ実行する

```sh
just check-containers
just check-container linux/amd64
just check-container linux/arm64
```

並列ビルドはCPUとメモリを多く使います。Dockerのリソースが不足する場合は、
`just --jobs 1 pre-push`または`just --jobs 1 check-containers`で逐次実行に戻せます。
`bash scripts/smoke-containers.sh`を直接呼ぶ場合も、指定した環境を順番に検証します。

Justfileを変更したときは`bash scripts/test-pre-push.sh`で、並列実行、ログ、
失敗の検出、後片付けの完了待ち、逐次実行を確認できます。このテストは`just check`にも含まれ、
Dockerを起動せずに実行できます。

ローカルでの成功だけでは、GitHub Actionsや実機AMD64での成功は確認できません。
push後はCIの結果も確認してください。
Foundation checksワークフローは`Swatinem/rust-cache`でCargoの依存ビルドを復元し、
保存は`main`の実行だけが行います。ブランチとプルリクエストは最新の`main`のキャッシュを再利用します。
同じブランチやプルリクエストに新しいpushがあると前の実行は取り消されますが、`main`ではすべて実行します。
CIはリンク時間を短くするため`CARGO_PROFILE_DEV_DEBUG=line-tables-only`でデバッグビルドを行い、
Windowsのテストは一時ディレクトリに`RUNNER_TEMP`を使います。
リリースワークフローのネイティブテストも同じ2つの設定を使いますが、Cargoのキャッシュは使わず、
配布アーカイブは常にクリーンなtargetディレクトリからビルドします。

Markdownファイルと`docs/`だけを変えた場合は、Foundation checksを実行せず、Documentation checksワークフローを実行します。
このワークフローは`scripts/check.sh`と同じ`scripts/check-docs.sh`を使い、機能台帳と生成した機能表、
リリースノートのリンク、Docker Hubの説明文を検査します。コミット済みのドキュメントを読む検査を追加するときは、
両方のワークフローで実行されるよう、このスクリプトに加えます。ほかのファイルも変えた場合は通常どおりFoundation checksを実行し、
リリースのワークフローは常にすべての検査を実行します。ドキュメントだけのコミットでFoundation checksを実行したいときは、手動実行を使います。
依存の更新確認や直接実行するコマンドは[英語の開発手順](../en/development.md)を参照してください。
