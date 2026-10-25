# 導入・移行トラックの調査メモ

<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->

[実装計画](onboarding-tracks-plan.ja.md)の前提にした調査の記録です。計画の決定と手順は計画書の側にあり、
この文書は根拠を確かめたいときに読みます。調査日は **2026-10-24**、対象は `main` の `9003939` 時点のソースです。
行番号は変わるので、関数名で探してください。

## 1. Distribution

| 項目 | 現状 | 根拠 |
| --- | --- | --- |
| リリースノート | `docs/en/releases/` が存在しない。タグ実行は `docs/en/releases/$version.md` を必須にしている | `.github/workflows/binaries.yaml` の `validate` ジョブ |
| Release binaries workflow | 一度も実行されていない（手動 dispatch・タグとも履歴なし） | `gh run list --workflow binaries.yaml` が空 |
| GitHub Release | なし | `gh release list` |
| GHCR | 未公開。初回公開時はパッケージが private で作られ、オーナーが Public に変更して manifest ジョブを再実行する | `docs/en/container-distribution.md` の「First publication and access」 |
| Foundation CI | `9003939` で成功。`containers` ジョブは `ubuntu-24.04`（native amd64）と `ubuntu-24.04-arm` で smoke を実行 | `.github/workflows/ci.yaml` |
| ローカルイメージ | 2026-10-24 に arm64（native）と amd64（エミュレーション）で smoke・README 手順が成功 | `docs/en/evidence/container-onboarding.md` |
| 利用者ガイド | README と `docs/en/container-usage.md` は `ghcr.io/shiroemons/gcs-mimic:0.1.0` を「予定」として記載 | 両ファイル冒頭 |
| Ruby SDK | CI では実行していない（`just check-sdk` の `--with-ruby` はローカルのみ） | `Justfile`、`ci.yaml` |
| 資源測定 | `DESIGN-079`（RSS / cgroup メモリ）は NOT_STARTED。`release-verification` の完了条件に resource evidence がある | 台帳 |

`DESIGN-059` の注記は「通知ファイルを含む現行イメージの native amd64 実行が未確認」としているが、最新の
Foundation run の `containers` ジョブは現行 Dockerfile で native amd64 smoke を実行している。

## 2. 設定と起動

### 設定の読み込み

- 設定は `src/config.rs` の `Config::load(args, env)` に集約されている。優先順位は CLI > `GCS_MIMIC_*` 環境変数 >
  JSON ファイル（`--config`、64 KiB、値は文字列か符号なし整数だけ）> 既定値。
- 未知のキーは `unknown setting: <key>` で、**最初の 1 件で**失敗する。同じ CLI オプションを 2 回指定してもエラー。
- CLI は `--name VALUE` の組だけを受け付ける。`-scheme` のような 1 本のハイフンは
  `serve options require --name VALUE pairs` になり、正しい書き方は案内されない。先頭の引数が `serve` などの
  既知のコマンドでなければ `invalid command` になる（`src/main.rs` の `run`）。
- `src/main.rs` は `GCS_MIMIC_` で始まる環境変数**だけ**を集めて `Config::load` に渡す。`FAKE_GCS_*` は届かないので
  黙って無視される。
- `GCS_MIMIC_PORT` をサーバーに渡すと `unknown setting: port` になる（Compose のホストポート変数であることは出ない）。
- イメージの `CMD` は `serve --listen 0.0.0.0:4443 --data-dir /var/lib/gcs-mimic` で、`seed_dir` を指定していない。
  上流のイメージの `-data /data` に相当するものはなく、`/data` をマウントしても使われない。

### 通知する URL

- `api_base_url` は明示しない場合 `{scheme}://127.0.0.1:4443` で、`listen` のポートを反映しない。
  `selfLink`・`mediaLink`・resumable の Location（`src/server/resumable.rs`）はすべてこの値から作られる。
  Host や forwarded ヘッダーは使わない。
- `compose.yaml` はホスト側のポートを `${GCS_MIMIC_PORT:-4443}` で変えられるが、`GCS_MIMIC_API_BASE_URL` の既定は
  `https://localhost:4443` で固定。ポートを変えると通知 URL がずれる。
- サーバーは公開 URL を生成しない。公開 URL は `url public` / `url sign` の `--base-url` でだけ指定でき、両ヘルパーの
  ヘルプは「設定ファイル・環境変数・データディレクトリ・ネットワークを読まない」と約束している。
- 結合テストの多くは `127.0.0.1:0` で起動し、`tests/common/mod.rs` の `http_config()` などで `api_base_url` を明示している。
  既定値の変更で影響を受けるのは主に `src/config.rs` の単体テスト、`main.rs` のヘルプ文、利用ガイド。

### Host とルーティング

- path-style の XML（`xml::object_path`）は Host を照合しない。予約済みの名前空間（`/storage/v1`、`/upload/storage/v1`、
  `/download/storage/v1`、`/batch/storage/v1`、`/_internal`）以外の `/{bucket}/{object}` を任意の Host で受け付ける。
  対応メソッドは GET・HEAD・PUT だけ。
- 仮想ホストは `xml_virtual_host_domains`（DNS 接尾辞、最大 32 個、長い順に照合、IP は `validate_dns_name` が拒否）。
  `xml::authority_error` は、仮想ホストを使うときに Host とリクエスト先が一致するかだけを見る。`api_base_url` の
  ホストが仮想ホストに該当すると起動を拒否する。
- `json::route_with_profile` は `seed_project` を受け取り、`legacy-emulator` で `project` のないバケット作成の
  既定プロジェクトに使う。

### listener とログ

- `server::serve_with_tls` は listener 1 本と TLS acceptor 1 つを受け取り、`max_connections` もその 1 本で数える。
  `Config::load` は `scheme=http` で TLS 設定があると拒否する。ready event の `listen` も 1 つ。
- ログの仕組みはない。stdout に JSON の `ready` / `stopped`、stderr に JSON の `seeding` / `bind_wait` と、
  約 25 か所の自由書式の `eprintln!` がある。テストと smoke は stdout の ready event を解析している。

### 起動とシャットダウン（`src/main.rs` の `serve`）

1. `GCS_MIMIC_` 環境変数の収集 → `Config::load` → TLS 素材の読込 → ID レジストリの読込（ランタイムの外）。
2. ランタイムを作り、シグナルを登録する。
3. `Catalog::open_with_clock`: ロック取得 → 読み取り専用でのスキーマ検査 → WAL への切り替え → v0 からの新規作成
   または段階的な移行 → `rewrite::recover_due`・`resumable_maintenance::recover_due`・`objects::recover`。
   `objects::recover` は内部で `resumable_recovery::recover` と `derived::recover` を呼んだ後、staging・削除ジャーナル・
   blob を確認し、参照されていない staging エントリと blob を削除し、`blobs/` の想定外のエントリでは失敗する。
   新規カタログは `startup_seed` を未完了で作る。
4. `configure_public_access_prevention` → `seed::import`（失敗時はカタログを閉じて終了）。
5. シグナルと競わせながら bind → `start_resumable_maintenance` → `State { ready: true }` → ready event →
   `serve_with_tls`（終了時に `ready=false`、listener を閉じ、drain し、カタログを閉じる）。

問題点:

- PAP 設定の失敗時と bind の失敗時は、カタログを明示的に閉じずに終了する。
- shutdown と競わせているのは bind だけ。カタログの open・復旧・PAP・seed の間は SIGTERM を受けても終了しない。
  `docker stop` は 10 秒後に SIGKILL。イメージと `compose.yaml` の healthcheck は start-period 2 秒・間隔 2 秒・15 回で、
  約 32 秒しか待たない。

## 3. カタログと公開経路

### 受付枠

| 枠 | 数 | 取り方 | 溢れたとき |
| --- | --- | --- | --- |
| `admission` | 32 | `Catalog::run` が `try_acquire` | `catalog operation limit reached` で即失敗 |
| `execution` | 1 | 待機 | SQLite の接続は 1 本で、すべて直列 |
| `uploads` | 8 | `admit_object_upload` が `try_acquire` | `upload limit of 8 reached` で即失敗 |

公開の確定処理（`run_upload`）は `admission` を通らず `execution` を待つ。`get_object`・`open_object`・一覧などの
読み取りは `run_object` → `Catalog::run` を通る。`Catalog::close` は `admission` を閉じ、`uploads` の 8 枠と
`execution` を取ってから接続とロックを解放する。`is_healthy` も既存の枠だけを見る。

### 管理用の書き込み経路

- seed（`src/seed.rs`）は `Catalog::insert_bucket`、`get_bucket`、`check_object_upload`、`put_object` を
  `BucketAccess::Unchecked` で使っている。
- `put_object` は content type 以外のメタデータを受け取らない（`put_object_prechecked` は `pub(crate)` で受け取れる）。
  世代は `max(現在時刻のマイクロ秒, last_generation + 1)`。
- `open_object` は不変 blob を開いた `ObjectReader`（64 KiB 単位）を返す。非バージョニングの上書きは確定直後に古い blob を
  削除するが、開いた後なら Unix では読み続けられる。
- `insert_bucket` は `ON CONFLICT(name) DO NOTHING` で既存なら `AlreadyExists`。既定の IAM ポリシー
  （`Policy::default_for(project)`: プロジェクトの owner / editor / viewer への legacy ロール）と `bucket_incarnations` を
  同じトランザクションで作る。管理経路で作ったバケットにも strict の認可は API で作った場合と同じに効く。

### 書き込み経路の重複

`objects` への INSERT と世代の割り当ては 3 か所に別々に書かれている。

| ファイル | 経路 |
| --- | --- |
| `src/storage/objects.rs` の `put_object_prechecked` | media・multipart・XML PUT・seed |
| `src/storage/derived.rs` の公開処理 | copy・rewrite・compose |
| `src/storage/resumable_transfer.rs` の確定処理 | resumable |

ほかの書き込み経路: `update_object_metadata_authorized`・`delete_object_authorized`・`retire_previous`、compose の
元オブジェクト削除（`delete_derived_source_in_transaction`）、`insert_bucket`・`update_bucket`・
`delete_bucket_authorized`、`policies.rs` の `set_policy`（`bucket_policies` の 1 行を更新）。

### 削除の制約

- イベントベースの保留中のオブジェクトは削除できない（`ActiveHold`）。
- バケットの削除は、非 current の世代も含めて行が 1 つでもあると `NotEmpty`。成功すると CORS・IAM も消える。

### スキーマ

- `user_version = 13`、テーブルと索引は 17 個（空のデータディレクトリでサーバーを起動して確認）。
- `validate_schema` は版ごとの DDL 定数から組み立てた期待値と `sqlite_schema` の一覧を比べるが、一覧は `LIMIT 32`。
- `validate_schema` は `object_generation.last_generation >= max(generation)` を検査している。
- 変更ジャーナルに当たる仕組みはない。

## 4. バケット作成の検証（Initializer の前提）

- `src/json/buckets.rs` の `parse_insert_with_profile` が扱うのは `name`、`location`（`US` / `EU` / `ASIA`）、
  `storageClass`、`labels`、`cors`、`versioning.enabled`、`defaultEventBasedHold`、`iamConfiguration.publicAccessPrevention`。
- `UNSUPPORTED_FIELDS` の 17 個は 501 相当で拒否するが、**それ以外の未知フィールドは黙って無視する**。
  `lifecycle: {"rule": []}` は pinned Go SDK のための例外として通す。
- `legacy-emulator` では `labels` を捨て、`location` を `US`、`storageClass` を `STANDARD` に固定し、名前の検証を緩める。
- API のバケット本文の上限は 64 KiB（`BUCKET_BODY_LIMIT`）。

## 5. オブジェクト名（Sync の前提）

`json::validate_object_name` が拒否するのは、空・1024 バイト超・CR / LF・`.` と `..` そのもの・
`.well-known/acme-challenge/` で始まる名前だけ。NUL、`a//b`、先頭や末尾の `/`、`a/./b`、`a/../b`、バックスラッシュ、
Windows の予約名、大文字小文字だけが違う名前は API で作れる。macOS（既定の APFS）と Windows は大文字小文字を
区別せず、macOS は Unicode の正規化も区別しない。

## 6. 上流 fake-gcs-server（v1.56.1、`896ece51e49eed0946089f3089f6343855c0d106`）

### 設定とルーティング

- 19 個の環境変数と既定値は [configuration reference](../docs/en/fake-gcs-server-configuration.md) にある。
- 公式イメージのエントリーポイントは `-data /data` を自動で渡し、seed は起動のたびに走って同名オブジェクトを上書きする。
- `-public-host` は `publicHostMatcher` で Host の照合に使われる。設定値にポートがなければ、リクエストの Host のポートを
  除いて比べる。`/storage/v1` の JSON ルートは Host を問わない経路と publicHost 経路の両方にある。
- `{bucket}.{publicHost}` の仮想ホスト、publicHost での path-style の取得・アップロード・削除に加え、
  `Host("{bucketName:.+}")` という任意の Host をバケット名とみなす GET/HEAD/PUT のフォールバックがある。
  XML の一覧、DELETE、フォームアップロードもある。
- `URL()` は `-external-url` を優先し、`PublicURL()` は `{scheme}://{publicHost}`。
- `-scheme both` は HTTP と HTTPS の 2 つの listener を作り、同じオプションを共有する。

Host 照合の細部（ポートの扱い、フォールバックとの優先順位）はソースだけでは確定できない。
`scripts/compare-reference-emulator.sh` は上流をソースからビルドして同じリクエストを流す仕組みだが、
今あるのはバケット操作のケースだけ（実行には `GCS_MIMIC_REFERENCE_REPO` と Go が必要）。

### seed ディレクトリ（`fakestorage/server.go` の `generateObjectsFromFiles`）

- 最上位のディレクトリがバケット。最上位のファイルと読めないバケットは黙ってスキップする。
- `filepath.Walk` の通常ファイルだけをオブジェクトにする。本文は全体をメモリに読む。空のバケットも作る。
- Content-Type は Go の `mime.TypeByExtension`。`main.go` の `addMimeTypes` が `.yaml` / `.yml` を追加する。公式イメージは
  `alpine:3.24.1` に `mailcap` を入れているので `/etc/mime.types` も使われ、結果が実行環境で変わる。
- 各オブジェクトに ACL `projectOwner-test-project: OWNER` を付ける。

### filesystem backend（`internal/backend/fs.go`、`object.go`、`bucket.go`、`metadata_xattr.go`、`metadata_file.go`）

- バケットディレクトリ名は `url.PathEscape(bucket)`、オブジェクトは名前をそのままパスにする。
- xattr とサイドカーはルート全体で 1 回選ぶ。xattr が使えれば `user.metadata`、使えなければ `<object>.metadata`。
  Windows ではサイドカーになる。
- `<bucket>.bucketMetadata` に入るのは `DefaultEventBasedHold`、`VersioningEnabled`、`ACL` だけ。作成時刻は
  ディレクトリの stat から読むので、コピーすると失われる。
- `ObjectAttrs`: `StorageClass`、content 系ヘッダー 5 つ、`Crc32c`・`Md5Hash`（base64）、`Etag`、`ACL`、`Metadata`、
  `Created`・`Deleted`・`Updated`・`CustomTime`（文字列、形式は `2006-01-02T15:04:05.999999Z07:00`）、`Generation`、
  `RetentionMode`・`RetentionRetainUntil`。
- 世代は `time.Now().UnixNano() / 1000`（gcs-mimic と同じマイクロ秒）。filesystem backend はバージョニングを拒否する。
- チェックサムは指定があればそのまま保存する（本文と一致する保証はない）。
- 名前が `/` で終わるオブジェクトはディレクトリを作るだけで、本文も属性も残らない。
- 外部での編集は反映されない。上書きしてもハッシュと世代は古いまま、新しい inode に置き換えると xattr が失われて
  404 になる（[storage investigation](../docs/en/fake-gcs-server-storage.md)）。

## 7. 依存の追加手順

`compatibility/dependency-status.json` の `direct_dependencies` に、版、ライセンス、圧縮サイズ、最低 Rust 版、目的、
feature、保守状況、出典 URL、SHA-256、解決の制約を書く。配布物の通知ファイルは `cargo xtask release notices` が
依存グラフから集め、ライセンスファイルのないクレートでは失敗する（`xtask/src/release.rs`）。日次の更新検出の対象にも入る。

## 8. 共通化の候補

| 処理 | 重複している場所 | 補足 |
| --- | --- | --- |
| 世代の割り当てと `objects` への INSERT | `objects.rs` の `put_object_prechecked`、`derived.rs` の公開処理、`resumable_transfer.rs` の確定処理 | 3 か所とも `max(マイクロ秒, last_generation + 1)`、`retire_previous`、INSERT、確定後の `reclaim` を同じ順で行う |
| MD5・CRC32C の計算 | `objects.rs` の `write_body`、`derived.rs`（転送中の計算と、再開時の検証の 2 か所）、`resumable_transfer.rs` の確定処理 | derived は MD5 の途中状態を `md5_state` に保存して再開する。resumable は確定時に staging 全体を読み直してハッシュを計算するので、大きなアップロードでは本文を 2 回読む |
| 上限付きのファイル読み込み | `config.rs`（`take(65_537)`）、`auth/mod.rs`（`MAX_REGISTRY_BYTES + 1`）、`tls.rs`（`PEM_LIMIT + 1`）、`main.rs` の秘密鍵（`take(16385)`） | 同じ「上限 + 1 バイト読んで超過を検出」の書き方 |
| 基底 URL の検証 | `config.rs` の `api_base_url` の分岐と `url::normalize_base_url` | 検査内容（scheme、資格情報、query、fragment、末尾の `/`）が同じ |
| JSON の event | `main.rs` の `ready` / `stopped` / `bind_wait`、`seed.rs` の `seeding` | それぞれ手書きの `println!` / `eprintln!` |
| ディレクトリ走査と OS のメタデータの判定 | `seed.rs` の `entries` と走査、`objects.rs` の `os_metadata` | 走査の上限（10000 件、32 階層）と `.DS_Store`・`._*` の判定 |
| HTTP クライアント | `main.rs` の `healthcheck` と `probe_response` | TLS・CA・2 秒の期限・16 KiB の応答上限を持つ最小の GET クライアント |
| 16 進数への変換 | `objects.rs`（blob ID の生成）、`derived.rs`、`rewrite_token.rs` | 既存箇所は1バイトごとに `format!("{byte:02x}")`。`import-dry-run` が `crate::hashing::encode_lower` を作成済みで、`shared-blob-id` が既存箇所を載せ替える |

性能と堅牢性に関わる点:

- 公開 1 件あたりの fsync: 本文の `sync_all` → `publish_blob` の `sync_dir(blobs)`（シャードが既にあっても毎回）→ rename →
  `sync_dir(shard)` → WAL のコミット（`synchronous = FULL`）。上書き時は、コミット後の `reclaim` が `run_upload` の中、つまり
  `execution` を握ったまま、unlink → ディレクトリの fsync → `DELETE FROM pending_blob_deletions` を行う。
- 起動時の `objects::recover` は、参照されているすべての blob を stat し、`blobs/` のすべてのファイルについて参照を 1 件ずつ問い合わせる。
  件数に比例し、bind 前に走る。
- `std::fs::copy` は書き先を fsync しない。
- seed（`src/seed.rs`）は `os_metadata` を使わない。`docs/en/slices/seed.md` は「`.DS_Store`・`._*` もバケット内では普通の
  オブジェクト、最上位にあれば失敗」と意図して決めている。
- seed は `Vec` のバッファに読んでから 64 KiB ごとに `Bytes::copy_from_slice` する。`server.rs` の本文の受信も、大きなフレームを
  保持し続けないため、常に 64 KiB ごとにコピーしている。

型とエラー:

- `BucketAccess` は `#[derive(Default)]` で既定値が `Unchecked`。`Unchecked` は permissive の API と、seed の管理操作の両方を表す。
  `Policy` の `permission` は `String`。`Catalog::put_object`・`open_object`・`check_object_upload` は `pub` で `Unchecked` を使う。
- `CatalogError::Unavailable(String)` と `ObjectError::Unavailable(String)` に、I/O・枠の不足・クローズ・破損が文字列で入る。
  `io::ErrorKind::StorageFull` を区別する箇所はない。`src/` で `Result<_, String>` は 20 か所。
- バケット名・オブジェクト名は `String`、世代は `i64`、blob ID は `String`（`blob_path` が毎回検証する）。
- `serde` と `serde_derive`（1.0.229）、`libc`（0.2.191）は既に依存グラフにある（`Cargo.lock`）。`rustix` と `proptest` はない。
- `Cargo.toml` に `[lints]` はなく、`scripts/check.sh` の `cargo clippy -- -D warnings` が lint の唯一の設定。unsafe は
  Windows の `MoveFileExW` とテストの箇所にある。

ほかの性能に関わる点:

- `max_blocking_threads` の既定は 2。`Catalog` の処理（`run`、`run_upload`）は `spawn_blocking` で、`tokio::fs` も同じプールを使う。
- resumable の確定は staging 全体を読み直してハッシュを計算する（上の表）。共通のハッシュ計算に途中状態の保存を持たせれば
  読み直しをなくせるが、resumable は今回のトラックの対象外なので、提案に留める。

## 9. 調べた範囲と未確認の点

| 深さ | 対象 |
| --- | --- |
| 全体を読んだ | `src/main.rs`、`src/seed.rs`、`src/config.rs` の `Config::load`、`src/server.rs` の `State`・`serve_with_tls`・`serve_connection`・`respond`・`respond_routed`、`src/xml/mod.rs` の認証・パス解釈・`route`、`src/storage/catalog.rs` の `validate_schema`・`open_with_clock`・`close`・`run`・`insert_bucket`・`update_bucket`・`delete_bucket_authorized`、`src/storage/objects.rs` の復旧・blob 公開・`put_object_prechecked`・取得・メタデータ更新・削除・実行枠 |
| 該当部分を読んだ | `src/json/buckets.rs`、`src/json/query.rs` のバケット作成ルート、`src/json/objects.rs` の `validate_object_name`、`src/storage/derived.rs` の公開処理、`src/storage/resumable_transfer.rs` の確定処理、`src/storage/compose.rs` の元オブジェクト削除、`src/storage/policies.rs` の `set_policy`、`src/server/resumable.rs`、`src/auth/mod.rs`、`src/auth/policy.rs`、`src/url.rs`、`tests/common/mod.rs`、`xtask/src/main.rs`、`scripts/smoke-containers.sh`、`scripts/compare-reference-emulator.sh`、`Dockerfile`、`compose.yaml` |
| 実際に動かした | 空のデータディレクトリで HTTP サーバーを起動・停止し、カタログのテーブル・索引と版を確認 |
| 上流 | 上記の各ファイル、`main.go`、`Dockerfile`、`internal/config/config.go`、`internal/backend/memory.go` の時刻形式 |

未確認:

- 公開処理の共通化が `src/storage/derived.rs` の復旧と rewrite の継続トークンに与える影響。
- PAP の親ポリシー（`configure_public_access_prevention` の一時テーブル）を変更ジャーナルに含めるか。
- 上流の `-public-host` の Host 照合の実際の挙動。
- Windows でのファイル監視（rename の原子性は、計画書で「Windows は競合の時間窓を契約に書く」と方針を決めた）。
- memory backend の内部構造（稼働中の状態のエクスポートで必要。このトラック群の対象外）。
- `cargo test` は計画の作成とレビューの過程で実行していない。

## 10. 実利用アプリの移行調査（2026-10-25）

fake-gcs-server を使っている実在のアプリ 1 件（Go の API とワーカー、Ruby の管理画面、ブラウザからの直接アップロード、
画像プロキシ）の利用状況を読み、同じ形のリクエストを main（`f9b8833`）の debug ビルドに送って確かめた。アプリ固有の名前は書かない。
計画は [計画書 第 9 節](onboarding-tracks-plan.ja.md#9-fake-gcs-server-からの移行実利用の検証)。

### 利用の形

- 起動: `-scheme http -public-host gcs:4443 -external-url http://gcs:4443 -data /data -filesystem-root /storage`。
  healthcheck なし。`/data` には空のバケットディレクトリだけを置き、作業ツリーの起動スクリプトがそれを `mkdir -p` する。
- 接続: コンテナ間は `STORAGE_EMULATOR_HOST=gcs:4443`、ブラウザ向けにはアプリ独自の変数で `http://localhost:{割り当てたポート}`。
  作業ツリーごとにホスト側のポートが変わる。
- Go SDK: `NewWriter`（resumable の Location が届かない問題の回避として `ChunkSize = 0`）、`CopierFrom` + 元の世代指定 +
  `DoesNotExist`、`Attrs`（CRC32C・世代）、`NewReader`、世代指定の `Delete`。
- 生の HTTP: resumable セッションの作成と `DELETE` での取り消し。サーバー側で作ったセッション URI のホストを、ブラウザ向けの
  オリジンに書き換えて渡す（同じセッションを別の Host で使う）。
- Ruby SDK: `endpoint` を指定した anonymous クライアントで `bucket.file`、`create_file`、`delete`。
- ブラウザ: 別オリジンから `uploadType=media` の URL へ `POST`、resumable のセッション URI へ `PUT` + `Content-Range`（1 回で全体）。
  複数ファイルを並行に送る。表示は `?alt=media`、保存済みの URL は path-style（`http://gcs:4443/{bucket}/{object}`）。
- エミュレーター使用時は署名付き URL を作らず、`uploadType=media` の URL を代わりに返している。

### 実際に送った結果

| 操作 | 結果 | 移行への影響 |
| --- | --- | --- |
| 起動（HTTP、空のバケットディレクトリ 2 つを seed） | 約 0.2 秒で ready、バケット 2 つを作成 | なし |
| `GET /storage/v1/b`（project なし） | 400 `Missing required project`（`legacy-emulator` でも同じ。作成は seed_project を補う） | 開発者の確認コマンドが失敗する |
| 別オリジンの preflight と `POST uploadType=media` | 200、CORS 応答あり | なし |
| resumable 作成 → Host を変えて `PUT` + `Content-Range`（preflight 含む） | 200 | なし |
| path-style（`Host: gcs:4443`）と `?alt=media` の匿名読み取り | 200 | なし |
| rewrite（`sourceGeneration` + `ifGenerationMatch=0`）、同じ操作の再実行 | 成功、再実行は 412 | なし |
| 世代指定の削除、resumable の `DELETE` | 204、499 の後 410（Google と同じ） | なし |
| 4 MiB のアップロードを 12 本同時 | **8 本成功、4 本が即 503**（`backendError`、`Object storage is unavailable`、Retry-After なし） | **移行を止める**。ブラウザの並行アップロードとワーカーが失敗する。fake-gcs-server に上限はない |
| 遅いダウンロードを 40 本同時（`max_connections` 32） | 全件 200（待たされる） | なし |
| 128 MiB のアップロード 8 本の最中に、別の接続で一覧と GET | 全件 200。応答は約 1 ms → 4〜8 ms（1 回の簡易確認） | なし。本文の書き込み（`tokio::fs`）とカタログ（`spawn_blocking`）が同じブロッキング用のプール（既定 2 本）を使うため、取り合いはあるが小さい |
| fake-gcs-server の command をそのまま渡す | 起動せず、5 フラグすべてに置き換え先つきの診断 | 診断は良い。Compose の書き換えは必要 |
| イメージの既定 HEALTHCHECK | `https://localhost:4443` 固定 | HTTP で動かすと上書きしない限り unhealthy |
| アイドル時の RSS（debug ビルド） | 約 36 MB | なし（参考値） |

### 実際の GCS の並行アップロードの制限

公式ドキュメント（3 ページとも 2026-10-22 更新版を 2026-10-25 に確認）。GCS には同時アップロードの**本数**の上限がなく、
制限はリクエストの**頻度**に対してかかる。

| 対象 | 公式の記述 | 出典 |
| --- | --- | --- |
| 同時アップロードの本数 | 上限の記載なし（同じオブジェクトへの multipart uploads も Unlimited） | [Quotas & limits](https://docs.cloud.google.com/storage/quotas) |
| 同じオブジェクト名への書き込み | 1 秒に 1 回。超えると throttling errors になりうる | [Quotas & limits](https://docs.cloud.google.com/storage/quotas) |
| バケットの書き込み・読み取り | Unlimited。初期は毎秒約 1,000 書き込み・約 5,000 読み取りで、必要に応じて自動で増える。超える場合は「20 分で 2 倍」より速く増やさない | [Quotas & limits](https://docs.cloud.google.com/storage/quotas)、[Request rate](https://docs.cloud.google.com/storage/docs/request-rate) |
| resumable セッション | 本数の上限の記載なし。開始から 7 日で期限切れ | [Quotas & limits](https://docs.cloud.google.com/storage/quotas) |
| バケットのメタデータ更新 | 1 バケットにつき 1 秒に 1 回 | [Quotas & limits](https://docs.cloud.google.com/storage/quotas) |
| 再試行 | 408・429・5xx は一時的な失敗として指数バックオフで再試行する。`objects.insert` は `ifGenerationMatch` を付けたときだけ冪等で、Go SDK の既定（`RetryIdempotent`）は条件のないアップロードを再試行しない | [Retry strategy](https://docs.cloud.google.com/storage/docs/retry-strategy) |

gcs-mimic の固定 8 本・即 503 は、GCS に対応する制限ではなく、エミュレーター自身の資源を守る内部の枠。
数本〜数十本の並行アップロードを GCS は断らない。しかも条件のないアップロードは Go SDK もブラウザの `fetch` も再試行しないので、
この 503 はそのまま失敗になる。頻度の制限（429 や 503）の再現は、既定の振る舞いではなく、台帳の AUDIT-009
（任意の quota モデル）と DESIGN-090（決定的な故障注入）で扱う。

### コードの根拠

- アップロードの枠: `src/storage/catalog.rs` の `uploads: Semaphore::new(MAX_UPLOADS)`（8）を `try_acquire` で取り、空きがなければ
  即失敗する（第 3 節「受付枠」）。設定で変えられない。エラーは利用者に原因と直し方を示さない。
- project の必須: `src/json/query.rs` の `project()`。`legacy-emulator` の補完は `POST`（作成）だけにある。
- resumable の Location: `src/server/resumable.rs` が `api_base_url` から作る。既定は bind したアドレスからの導出で、
  `0.0.0.0` は loopback になる。コンテナ内では `GCS_MIMIC_API_BASE_URL` を明示しないと、ほかのコンテナから届かない
  （上流で `-external-url` を省いたときと同じ落とし穴）。
- seed は新しいカタログのときだけ動く（`src/seed.rs`）。初回の起動の後に足したバケットディレクトリは作られず、知らせもない。
  上流は起動のたびに作る。`--buckets-file` は毎回「無いものだけ作る」ので、こちらが代わりになる。
- HEALTHCHECK: `Dockerfile` が `--url https://localhost:4443/_internal/ready` を固定し、`compose.http.yaml` と README が上書きしている。

### 未確認

- 永続データ（`-filesystem-root`）の取り込み: 調べたアプリのデータは、削除後の空ディレクトリと、ディレクトリのない
  `.bucketMetadata` だけだった。`import fake-gcs-server --dry-run` がこの形を受け付けるかは試していない。
  macOS の Docker Desktop のバインドマウント上の xattr の扱いも未確認。
- Go SDK と Ruby SDK そのものでの実行（上の結果は curl で同じリクエストを送ったもの）。
- fake-gcs-server と同じ条件での速度・メモリの比較（方針により測定の仕組みは作らない）。
