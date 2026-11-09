# 開発とpush前の確認

[English](../en/development.md)

性能調査は[ボトルネックの探し方](performance-investigation.md)と
[調査結果・採否の一覧（英語）](../en/evidence/read-optimization-summary.md)を確認してから始めます。

開発中は`just check`、push前の最終ソースでは`just pre-push`を実行します。
コンテナのビルドと動作確認は、AMD64とARM64を並列に進めます。

## 準備

macOSまたはLinuxで、Bash、just 1.56.0以降、rustup、Go、jq、curl、
Node.js 24以降（npm付き）、uv 0.13.6以降を使います。
Ruby・Bundler・gemは[SDKテストの指定バージョン](../../sdk-tests/ruby/README.md)を
揃えてください。PythonはuvがPython SDKテスト用の指定バージョンを用意するので、
別途インストールする必要はありません。PHP SDKテストはDocker上で動くので、PHPも不要です。
Java・Maven・.NET SDKは、`sdk-tests/java/.java-version`・`.maven-version`と
`sdk-tests/dotnet/global.json`に書かれたバージョンを揃えてください。

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
2. Go・Ruby・Node.js・Python・PHP・Java・.NET・Rust SDKの全テスト。
   Java・.NET・RustはHTTP専用のため、`GCS_MIMIC_SDK_SCHEME=https`では`SKIPPED`と表示して飛ばします。
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
既定では通常の`TMPDIR`を使い、CIと同じ逐次実行にします。テストのファイル同期はそのまま行います。
macOSの同期は`F_FULLFSYNC`でシステムディスク全体を書き出すため、これがテスト時間の大半を占めます。
システムディスク上で並列に実行すると、期限を確かめるテストが遅れて失敗するため、逐次実行のままにします。
macOSでは`GCSM_TEST_RAMDISK=1`を指定すると、実行中だけRAMディスク（`GCSM_TEST_RAMDISK_MB`、既定は4096）を作って
`TMPDIR`にし、並列数の既定を4に上げます。
RAMディスクはメモリを固定で確保するため、既定にはしません。取り外しに失敗すると、手で取り外すまでメモリが戻らず、
マシン全体が不安定になることがあります。既定のままなら、失うのは時間だけです。
終了時には、RAMディスク上のファイルをまだ開いているプロセスを`SIGTERM`、続けて`SIGKILL`で止め（止めたプロセスは一覧で表示します）、
それから取り外します。使用中のディスクは通常の取り外しに失敗し、予備の`-force`では使用中のファイルごと外してしまうためです。
また、macOSではどの実行でも、後始末の前に強制終了された実行が残したRAMディスクを解放します。
残りものかどうかはボリューム名に含まれるラッパーのPIDで判断し、そのPIDがまだ動いていれば手を付けません。
RAMディスクを作れなかった場合は警告を出し、逐次実行で続けます。macOS以外では何も変えません。
時間に敏感な失敗はRAMディスクの有無のどちらでも出ることがあります。どちらで出た場合もテストの欠陥として扱ってください。
これらの設定に`GCSM_TEST_`接頭辞を使うのは、サーバーが`GCS_MIMIC_`で始まる変数を
すべて自分の設定として読み、知らない名前を拒否するためです。

## テストで時間を待つ

共有のCIランナーは数秒止まることがあります。決まった時間の`sleep`や、テスト自身の短い期限で状態を見に行くテストは、
マシンの速さで成否が変わります。状態の到達は`src/test_support.rs`の補助関数で待ってください。
`eventually`・`eventually_some`はブロックして待ち、`eventually_async`・`eventually_some_async`は`await`で待ちます。
10 msごとに確かめ、`HANG_GUARD`（60秒）を過ぎたときだけ失敗します。これは止まったときの安全装置で、速さの基準ではありません。
統合テストからは`common::eventually`と`common::input_fixtures::*`で使います。
非同期版は時計を止めたテストには使えません。ブロッキング処理の間に時計が安全装置を飛び越えるためです。

経過時間に上限を付けるのは、2つの仕組みを見分けるときだけにします。その場合は競合する期限を上限から十分離します。
たとえば、本文を待たずに拒否することを確かめるテストでは、無通信の期限を30秒にして、上限を10秒にします。
下限（`elapsed() >= ...`）は、遅いランナーでは長くなるだけなので問題ありません。

`cargo xtask lint`は、`src/`と`tests/`のテストファイル（`src/test_support.rs`を除く）にある`sleep(`と`Instant::now()`の呼び出しを数え、
`xtask/test-clock-baseline.txt`と比べます。ファイルごとの件数は増やせず、減ったら記録も下げる必要があります。
一覧は`cargo xtask lint --test-clock-baseline > xtask/test-clock-baseline.txt`で作り直し、差分が減少だけであることを確かめてください。

`just stress-tests`（`scripts/stress-tests.sh`）は、テストバイナリを並列に動かし、全CPUを空回りのループで埋めた状態で
Rustのテストを繰り返し実行します。回数は`GCSM_STRESS_ROUNDS`（既定は2）、並列数は`GCSM_STRESS_JOBS`（既定はCPU数）で変えられます。
毎日動く**Timing stress**ワークフローが、LinuxとmacOSでこれを実行します。
ここでの失敗は製品の不具合ではなく、決定的に直すべき時間依存のテストを示します。

## gRPC生成コードの検証

gRPCのprotoは通常CIでSHA-256を照合し、日次の依存検査で固定したprotocから再生成してコミット済みRustと比較します。
protocの固定値は`src/grpc/codegen/protoc.json`で管理し、日次検査は公式の最新安定版との差も検出します。
生成・protoc・SDKの取得・依存検査は`!cancelled()`で実行するため、protoc検査の失敗後も依存検査が動きます。
どれかが失敗すればジョブは失敗し、キャンセル時は残りの検査を止めます。
通常ビルドやリリースにはprotocを追加しません。上流APIの変更を自動発見する仕組みは未実装です。
ローカルでは`bash scripts/test-grpc-codegen.sh`でハッシュ不一致・生成物のずれの拒否を確認し、
protoc 36.2を用意して`CARGO_BUILD_JOBS=4 bash scripts/check-grpc-codegen.sh`で実際に再生成します。
固定版と配布アーカイブのSHA-256は2026-11-04に公式リリースで確認しています。

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
Windowsのテストは一時ディレクトリに`RUNNER_TEMP`を使います。
リリースワークフローのネイティブテストも同じ設定を使いますが、Cargoのキャッシュは使わず、
配布アーカイブは常にクリーンなtargetディレクトリからビルドします。

デバッグビルドとテストビルドは行番号テーブルだけを残します（ワークスペースの`[profile.dev]`の
`debug = "line-tables-only"`）。パニックとバックトレースの位置は残したまま、コンパイルとリンクを短くし、
`target/`を小さくします。テストのCPU時間の大半は依存クレートで使われるため、依存クレートだけを
`opt-level = 3`（`[profile.dev.package."*"]`）でビルドします。ライブラリのテストは約121秒から79秒に短くなり、
依存クレートは一度ビルドすればキャッシュされます。ローカルではCargo既定のインクリメンタルコンパイルを有効のままにしてください。
`src/`を1行変えたあとの`cargo test --workspace --no-run`は、`CARGO_INCREMENTAL=0`の約43秒に対して
約8秒で終わります（Apple Mシリーズ、14コア）。Cargoは以前のコンパイラ、フラグ、フィーチャーで作った
インクリメンタルの状態を削除しないため、`just check`は最初に`scripts/trim-target.sh`
（`just trim-target`でも実行可）を実行します。このスクリプトは、`GCSM_TARGET_INCREMENTAL_DAYS`日（既定3日）
どのビルドも触れていないインクリメンタルのクレートディレクトリを削除します。それでも`target/`が
`GCSM_TARGET_MAX_MB`（既定30720）を超える場合は、インクリメンタルの状態をすべて削除し、
さらにデバッグとクロスチェックのビルド出力を削除します。CargoはCIを検出すると、既定でインクリメンタル
コンパイルを無効にします。

Markdownファイルと`docs/`だけを変えた場合は、Foundation checksを実行せず、Documentation checksワークフローを実行します。
このワークフローは`scripts/check.sh`と同じ`scripts/check-docs.sh`を使い、機能台帳と生成した機能表、
リリースノートのリンク、Docker Hubの説明文を検査します。コミット済みのドキュメントを読む検査を追加するときは、
両方のワークフローで実行されるよう、このスクリプトに加えます。ほかのファイルも変えた場合は通常どおりFoundation checksを実行し、
リリースのワークフローは常にすべての検査を実行します。ドキュメントだけのコミットでFoundation checksを実行したいときは、手動実行を使います。

Markdownの段落は途中で改行せず、1つの段落またはリスト項目を1行に書きます。そのため、ルートの`.markdownlint-cli2.jsonc`はMD013（行の長さ）を無効にしています。

`just check`は、Windows MSVC・Linux musl・macOS ARM64のリリースターゲットと、
CIのRustジョブが使うLinux glibcのターゲットも、`scripts/check-cross.sh`で型検査します。Foundation CIはこのスクリプトを`rust`ジョブと並行する`cross`ジョブで実行し、
`rust`ジョブは`GCSM_CHECK_CROSS=0`を指定して`scripts/check.sh`からの実行を省きます。
`just check`はホストのテストとほかのターゲットの型検査だけなので、時刻精度に依存するテスト、Linux専用のコード、
SDKスイートの失敗はCIで初めて出ることがあります。`cargo xtask lint`は[AGENTS.md](../../AGENTS.md)の実装規則を
本番コードに強制し、従来の規則は1件でも検出すると失敗します。インデックス、文字列スライス、切り詰め・符号を落とすキャスト、静的文字列エラーは、`xtask/lint-baseline*.txt`のファイル別件数の増加を拒否します。固定コンパイラはClippyの件数減少も拒否し、current-stableは減少を警告にします。共有テキスト規則の件数減少は常に拒否します。

ホストのClippyは`--target`を付けず、既存のネイティブ成果物を再利用します。ホスト検査ではCargoの既定ターゲットをネイティブにし、クロスビルドには`--target TRIPLE`を指定してください。登録済みのbaselineはmacOS ARM64・Windows MSVC・Linux GNU x86-64・Linux musl x86-64です。別ホストでも全規則を実行しますが、対象ファイルがなければ空のbaselineとして既存の検出も新規扱いで拒否します。検出内容を確認して`cargo xtask lint --baseline --target TRIPLE`で追加し、他ターゲットの件数を流用しないでください。形式と更新手順は英語版の[Per-file ratchet](../en/development.md#per-file-ratchet)を参照してください。

依存の更新確認、Linuxでのテスト手順、lintの規則、直接実行するコマンド、差分比較ハーネスは
[英語の開発手順](../en/development.md)を参照してください。
