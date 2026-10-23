# 開発とpush前の確認

[English](../en/development.md)

開発中は`just check`、push前の最終ソースでは`just pre-push`を実行します。
コンテナのビルドと動作確認は、AMD64とARM64を並列に進めます。

## 準備

macOSまたはLinuxで、Bash、just 1.56.0以降、rustup、Go、jq、curlを使います。
Ruby・Bundler・gemは[SDKテストの指定バージョン](../../sdk-tests/ruby/README.md)を
揃えてください。Pythonは不要です。

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
2. Go・Ruby SDKの全テスト。
3. AMD64・ARM64のコンテナ検証を並列実行。

各環境は、自分のビルドが終わり次第、動作確認へ進みます。
ログには`[linux/amd64]`または`[linux/arm64]`が付きます。
イメージ名、コンテナ、ボリューム、ホスト側のポートは環境ごとに分離されます。

失敗した段階から次の段階には進みません。コンテナ検証では両方の終了と後片付けを待ち、
片方でも失敗するとコマンド全体が失敗します。エラーを修正し、最終ソースで再実行してください。
このコマンド自体はコミットやpushを行いません。

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
依存の更新確認や直接実行するコマンドは[英語の開発手順](../en/development.md)を参照してください。
