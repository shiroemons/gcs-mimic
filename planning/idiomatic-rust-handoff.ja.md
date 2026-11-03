# Rust らしい実装への是正とテスト整理の引き継ぎ

<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->

2026-11-03 作成。この文書は、Rust らしくない実装の是正と、テストの重複削減の残作業を引き継ぐためのものです。対象は、全体レビューで見つかった指摘のうち、`main` の `c90b4ee5` 時点で未対応のものです。

実装状態の正本は [`compatibility/features.json`](../compatibility/features.json) です。この文書は状態・fidelity・証跡を変更しません。行番号は `c90b4ee5` 時点のもので、後の変更でずれます。着手するときは、`rg` で現在の位置を確かめてください。

## 1. 今回取り込んだもの

`7e155dbb..c90b4ee5`（85 コミット）を `main` に取り込みました。領域ごとに子ワークツリーで実装し、コミットごとに独立したレビューを通しています。

| 領域 | 主な変更 |
| --- | --- |
| gRPC・auth | `From<JsonError>` / `From<ObjectError>` / `From<CatalogError>` で `?` を使えるようにした。読み取りハンドル表を `State` ごとにした。v1 の一覧を JSON のルートから切り離し、旧来のクエリ長と URI 長の上限を保った。MD5 の `+0` を拒否。`SignUrlRequest`。16 進数の共通化（`crate::hex`） |
| 設定・起動・CLI・seed・import・sync・fs | `SeedError`、`ImportError`、`option_pairs`、`Setting` enum と整数の範囲の網羅的な定義、`InputKind::UnknownLegacy`、fsutil の `unix` / `unsupported` の分割。設定テストの表駆動化（`assert_precedence` / `assert_rejected`） |
| storage | appendable のスロットのキーを `StoreId` にした（アドレスの再利用で取り違える ABA の修正）。保存 JSON の serde 化（旧 encoder の出力をゴールデンで固定）。rusqlite の `fallible_uint`。保存用 enum の対応表。`BlobId` の型化、`StoredObject`、`BlobBackend`。resumable のディレクトリ fsync を fault のシームに通した |
| HTTP（json・xml・server） | sync 管理 API の応答上限を送信バイトで判定し、カーソルを型付きの codec にした。CORS の警告履歴を `State` ごとにした（`CorsWarnings`）。`JsonError` に種別（`ErrorKind`）を持たせ、種別ごとのコンストラクタを用意した。`XmlCode`。projection の enum をハンドラまで渡す |
| 結合テスト | ハーネスを `tests/common` に一本化した。64 個のバイナリを 8 個（`tests/it_*.rs`）にまとめた。`Reply::expect` / `expect_with`。`tests/` は差し引き約 1,100 行の削減 |

検証の結果は次のとおりです。

- **ホスト（macOS）**: `scripts/check.sh` のすべてのチェックとテストが成功した。クロスターゲットは除き、下で別に確認している。テストの内訳は、ライブラリ 1147 件、結合テスト 8 バイナリ、xtask など。失敗は 0 件で、ignore は既存の Pub/Sub の 1 件だけ。
- **クロスターゲット**: `scripts/check-cross.sh` で、Windows（msvc）・Linux（gnu・musl）向けの clippy と `xtask lint --target` が成功した。
- **未実施**: SDK スイート、コンテナのスモーク、Windows と Linux の実機でのテストの実行。

## 2. 意図した振る舞いの変化

どれもコミット本文に明記しています。後続の作業で、元に戻さないでください。

| 変化 | 理由 |
| --- | --- |
| 未知の保存 enum の値と、不正な `blob_id` を、`Unavailable` ではなく `Corrupt` に分類する。管理 API では `Backend` から `Corrupt` に変わる。`bucket_init` の診断は "catalog corrupt" になる。import はこの失敗を、再試行しない終端の失敗として扱う | 壊れた行は、再試行しても直らないため |
| `blob_id` の不正な値の原因の文言を "invalid opaque blob ID" に統一した。resumable の行で大文字の `blob_id` は `Corrupt` になる | 書き込み側は、すべて小文字の正準形で書いている |
| deletions のカーソルは、符号付きや大文字の 16 進を拒否する。旧実装では `-…1` が `after=-1` として 503 busy になっていた | ほかの 2 種類のカーソルと揃えるため |
| gRPC v1 は、RFC 3339 の範囲外の保存時刻でも 503 を返さない | v2 と揃うため。通常は到達しない |
| seed の診断を型で分類するようにした。ラベル（パス）に含まれる語で誤分類しない | 部分文字列の一致による誤分類を直した |
| 未知の `FAKE_GCS_LISTEN` で、誤った native の上書き警告を出さない | 警告のバグの修正 |

## 3. 進め方の教訓

次に同じ規模で進めるときに守ることです。

- **子ワークツリーの基点**: Orca の `--worktree new-child` は、統合ブランチの最新ではなく、元の基点から切られることがあった。worker は最初に `git merge --ff-only <統合ブランチ>` を実行し、`git log -1` で基点を確かめる。
- **クロスターゲットの clippy を worker の完了条件に入れる**: `cargo xtask lint` とホストの clippy だけでは、`cfg(unix)` 限定のコードによる Windows での unused や dead_code を拾えない。worker の完了条件は次の 2 つにする。
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `bash scripts/check-cross.sh`
- **並列で cargo test を回すと、時間依存のテストが落ちることがある**: 複数のワークツリーで同時に `cargo test` を回すと、fsync の競合で時間依存のテストが落ちることがある。失敗したら、まず `--test-threads=1` で単独で再実行し、初回の失敗と再実行の結果の両方を記録する。
- **sandbox の権限エラー**: loopback への bind が `PermissionDenied` で落ちるのは、worker の sandbox の制限によるもの。権限を許可して再実行する。
- **保存形式を変える前の準備**: 書き換える前に、旧実装の出力をゴールデンテストで固定する。値が入ったケースも含めること。期待値は、旧コミットに切り替えて、実際に出力したものを貼る。
- **durability の修正**: 修正前の実装で、`Fault::PowerLoss` のスナップショットのテストが落ちることを確かめてから直す。
- **コミット本文**: 振る舞いが変わる変更は、fix: に分け、本文に変化と影響を書く。本文にコミットハッシュを書かない。
- **履歴の書き換え**: 統合前のブランチに限り、`git commit --fixup=reword:<hash>` と `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash` で本文を直してよい。前後で `git diff` が空であることを確かめる。

## 4. 残作業

優先度の高い順に並べています。各項目には、現状・方針・受け入れ条件を書いています。

### 4.1 storage・json・gRPC にまたがる型付け

#### ストレージ障害の原因を文字列で判定している

- **現状**:
  - `src/json/error.rs:265` の `JsonError::backend(message: &str)` が、`BackendCause` の Display の文字列（`"catalog is closed"` など）を match して、利用者向けの案内を選んでいる。
  - `src/json/error.rs:904` の `safe_backend_cause` も、文字列の許可リストになっている。
  - 発生元は `src/storage/catalog.rs`（2 か所）、`src/server/copy.rs`（4 か所）、`src/server/resumable.rs`、`src/server/routing.rs`、`src/server.rs` の `Unavailable("…")` のリテラル。
- **問題**: 発生元の文言を 1 文字変えると、コンパイルは通るのに、汎用の案内に落ちる。
- **方針**:
  - `enum Overload { CatalogClosed, OperationLimit, UploadWaitTimeout, RetainedSessionLimit, OutboxLimit, SessionBusy, CopyDeadline, … }` を作り、`ObjectError` / `CatalogError` にバリアントとして持たせる。
  - json 側は、`Overload::guidance()` から案内を作る。
  - gRPC の変換も同じ型を見る。
- **受け入れ条件**:
  - HTTP / XML / gRPC の応答の文言とステータスが、バイト単位で同じであること。
  - 案内の文言を固定するテストがあること。
  - `JsonError::backend(&str)` と、文字列の許可リストがなくなること。

#### ACL のロールとストレージクラスが文字列のまま

- **現状**:
  - `AclEntry.role` は `String` で、`role == "OWNER"`（`src/acl.rs:88`）で認可している。
  - ロールの順位は `src/storage/catalog/acl/bucket_policy.rs:84` で手書きしている。
  - `"OWNER"` / `"WRITER"` / `"READER"` のリテラルは、生成コードを除いて 88 か所ある。
  - `storage_class` は、`src/bucket.rs:107`、`src/object.rs:48`、`src/storage/catalog/objects.rs:92`、`src/import/codec.rs:95` で `String` のまま。
  - `validate_storage_class`（`src/bucket.rs:368`）で検証したあとも、文字列のまま持っている。
- **方針**:
  - `enum AclRole { Reader, Writer, Owner }` を作り、`derive(Ord)` で順位を表す。
  - `enum StorageClass` を作り、`FromStr` / `as_str` を実装する。
  - catalog の TEXT 列と serde の境界は、`TryFrom` にする。
- **要判断**: 既存の DB に未知の値があった場合の扱い（`Corrupt` にするか、受け入れて保持するか）。`location_type == "region"` も同じ種類の問題。
- **受け入れ条件**:
  - ワイヤーと DB の保存形式が同じであること。
  - 未知の値を、決めた方針で扱うことを固定するテストがあること。

#### hold が `Option<bool>` の 3 状態

- **現状**: `src/object.rs:58-59`、`:223-224`。doc に「直接読まずに `has_hold` を使え」と書かれている。`src/storage/catalog/resumable/options.rs:27-28` も同じ。
- **方針**: `enum Hold { Unset, Released, Held }` を作る。JSON で、フィールドを省略するか false にするかの区別を保つ。
- **受け入れ条件**: JSON・XML・gRPC の往復と、保存 JSON（ゴールデン）が同じであること。

### 4.2 ガードレールの穴

#### `tokio::task::spawn_blocking` が禁止リストにない

- **現状**: `clippy.toml` の `disallowed-methods` に含まれていない。そのため、`crate::task::spawn_blocking`（`catch_job` を通す）を経由しない直接の呼び出しが残っている。
  - `src/server/batch.rs:71`
  - `src/server/ingress.rs:169`
  - `src/server/ingress/upload_gzip.rs:164`
  - `src/server/gzip.rs:156,166`
  - `src/server/unix.rs:33,132`
  - `src/bucket_init.rs:1021`
  - `src/startup.rs:1091`
  - `src/storage/catalog.rs:991,2488`
  - テストのものも含む
- **方針**:
  - `clippy.toml` に `tokio::task::spawn_blocking` と `tokio::runtime::Handle::spawn_blocking` を追加する。
  - 本番の呼び出しを `crate::task::spawn_blocking` に移す。
  - 移せない箇所（起動前や、runtime の外など）は、理由付きの `#[expect(clippy::disallowed_methods, reason = …)]` にする。
- **受け入れ条件**:
  - `cargo xtask lint` で違反が 0 であること。
  - panic が `JobPanicked` として表に出ることを、1 か所でテストすること。

#### `#[allow]` の残り

- **現状**:
  - 生成コードの取り込み口（`src/grpc/mod.rs:15`、`src/grpc/v1/mod.rs:12`）は、理由付きの例外として残す。
  - それ以外に残っているもの:
    - `#![allow(clippy::unwrap_used, …)]`: `src/diagnostics.rs:217`、`src/hashing.rs:145`、`src/match_glob.rs:283`、`src/storage/blob_id.rs:95`、`src/storage/catalog/admin/imported_identity.rs:111`、`src/storage/catalog/{admin/import_publication,admin/publication,change_journal,import_journal}/tests.rs:1`
    - `#[allow(dead_code)]`: `src/storage/catalog/test_support.rs:3`
    - `#[allow(unsafe_code)]`: `src/storage/catalog.rs:2319,2350`、`src/storage/catalog/objects.rs:2674,2875,2905,3001,3057`、`src/storage/catalog_tests.rs:1382`、`tests/common/mod.rs:133,137,161`（Windows 用）
  - `xtask/src/lint.rs:1295-1351` は、lint 自体のテスト入力なので対象外。
- **方針**:
  - 単独の `#[cfg(test)]` のモジュールの unwrap / expect は、`clippy.toml` の `allow-*-in-tests` で許可済みなので削除する。
  - プラットフォームで絞った cfg のモジュールは、理由付きの `#[expect]` にする。
  - storage のテストの unsafe（SQLite の進捗ハンドラと上限）は、rusqlite の `hooks` / `limits` feature の安全な API（`Connection::progress_handler`、`Connection::set_limit`）で置き換えられるか検討する。feature を dev 用だけで有効にできるか（features の統合の影響）も確かめる。
- **受け入れ条件**: `rg '#!?\[allow\(' src tests` が、理由付きの例外だけになること。

#### 診断ログの定型コピー

- **現状**: `crate::diagnostics::emit(…, json!({"message": format!("…{error}")}))` の形が、storage に 17 か所ある（`derived.rs`、`objects.rs`、`compose.rs`、`rewrite.rs`、`resumable_transfer.rs`、`resumable_maintenance.rs`、`blob_backend.rs`、`lock.rs`、`catalog.rs`）。
- **方針**: `diagnostics::warn_error(event, context, &dyn Error)` のような関数で、`context` / `error` / `source` を構造化したフィールドとして出す。既存の箇所を同じ変更ですべて移行する。
- **要判断**: ログの JSON の形が変わる。ログを読む利用者がいるなら、互換のために `message` を残すかどうかを決める。

### 4.3 HTTP の残り

| 項目 | 現状 | 方針 |
| --- | --- | --- |
| X-Goog-Hash の解析と生成 | 解析が `src/json/objects.rs`、`src/json/resumable.rs`、`src/xml/put.rs` にある。生成が `src/json/objects.rs:890`、`src/server/objects/read.rs:364`、`src/xml/mod.rs:976`、`src/xml/resumable.rs:256` にあり、remove してから入れ直す箇所もある | プロトコルに依存しない共有のモジュールにする（型付きの `GoogHashError`、`ObjectRecord` からハッシュのトークンを作る関数）。重複したアルゴリズムの扱いなど、アダプタごとの今の意味は呼び出し側で保つ |
| HTTP 日付の自作 | `src/json/buckets.rs:1164` の `http_date` | 依存にある `httpdate` を使う。範囲外の値で panic するので、先に範囲を検査する |
| 暦の計算の重複 | 定数 719468 が `src/json/buckets.rs:1209`、`src/auth/signature.rs:560`、`src/import/codec.rs:315` にある | `crate::clock` に `civil_from_days` / `days_from_civil` を 1 組だけ置く |
| 符号付きのエポックから SystemTime への変換 | `src/server/admin/fixtures.rs`、`src/import/filesystem.rs`、`src/xml/post.rs`、`src/auth/signature.rs`、`src/storage/catalog.rs` の `signed_system_time` | `crate::clock::from_unix(value, unit) -> Option<SystemTime>` にまとめる |
| ドメイン層が json に依存している | `object.rs`、`notification/event.rs`、`csek.rs` が、`crate::json::timestamp` / `decode_hash` / `base64` を使っている | json より下の層（clock・hashing・base64）に移す |
| 原因を捨てている箇所 | `src/server/objects/read.rs:166` の `map_err(\|_\| service_unavailable("Cannot seek object data"))` | `ObjectError` / `failure::unavailable` を経由させる。文言とステータスは同じに保つ |
| シリアライズの失敗が 400 になる | `src/server/admin/reset.rs`、`src/server/admin/reseed.rs` | 到達しない経路なので、`json_response_serialize` のようなヘルパーで吸収する |
| XML の書き込み応答のヘッダの選別 | `move_headers` の呼び出しが `src/xml/{put.rs:409,multipart.rs:370,post.rs:834,copy.rs:285}` で重複している | const の一覧とヘルパー 1 つにまとめる |
| 符号なし 10 進数の厳密な解析 | `is_ascii_digit` で検査してから parse する形が約 32 か所ある | `parse_decimal<T>` にまとめる。`+` を拒否する意味は保つ |
| 単一ヘッダのヘルパー | 4 実装ある: `src/json/resumable.rs:49`、`src/xml/put.rs:28`、`src/server/batch/parse.rs:66`、`src/auth/signature/v2/legacy.rs:21` | 共有の `single_header` にまとめ、各アダプタが自分のエラーに変換する |
| 小文字 16 進の検証 | インラインの検査と個別の関数が約 14 か所ある（`src/sync/mod.rs`、`src/sync_cli.rs`、`src/import/snapshot.rs`、`src/json/page_token.rs`、`src/server/admin/sync.rs`、`src/import/codec.rs` の `path_unescape` の `as u8` など） | `crate::hex` に `is_lower` / `decode` をまとめる。大文字を許すかどうかは、各箇所の今の挙動を保つ |
| ページトークンのコーデック | hex(JSON) と FNV-64 が 4 実装ある。FNV は `src/json/page_token.rs:245` と `src/json/projects.rs:390` の 2 つ | `token::seal` / `open` にまとめる。ワイヤーの互換性を保つ |
| エラーの原因チェーンのたどり方 | 4 通りある: `src/sync/mirror.rs:143`（`watch_retryable`）、`:191`（`is_transient`）、`:569`（`pause_reason`）、`src/server/admin/sync.rs:527`（`control_error`） | `io::Error` では `get_ref` を優先する `causes()` イテレータ 1 つにまとめる |
| ヘッダ名のリテラル | `"x-goog-…"` のリテラルが 188 か所ある | `HeaderName` の定数モジュールにまとめる。`HeaderName::from_static` に誤字があると、実行時に panic するため |
| サブモジュール内のタプル戻り値 | `src/json/objects.rs:554`、`src/json/resumable.rs:79`、`src/xml/put.rs:175` | `struct UploadHeaders` にする |
| 管理 API の sync のルート | パスを 2 回 match している。`src/server/admin/sync.rs:210` で、パスの比較結果を bool で持っている。`Page`（`:270`）が不正な状態を表現できる | `enum SyncRoute` / `enum PageQuery` で 1 回だけパースする |
| recovery_page の空回し | `src/server/admin/sync.rs:350` と `src/sync/mirror.rs:377` で、副作用のためにループしている | `refresh_recoveries` のような名前付きの関数を経由させる |
| 削除ガードのしきい値 | `src/server/admin/sync.rs:468` の `"min_objects":100,"percent":50` と、`src/storage/catalog/sync_state/deletions.rs` のリテラルが重複している | const を共有する |
| fixture のメタデータの往復 | `src/server/admin/fixtures.rs:195,292` で、Value をバイト列にしてから再パースしている | json 側に `&Map` を受け取る版を用意する |

### 4.4 storage の残り

| 項目 | 現状 | 方針 |
| --- | --- | --- |
| 管理用の cleanup が fault のシームを通っていない | `src/storage/catalog/blob_backend.rs:220-225` の `sync_blob_dir` が `crate::fsutil::sync_dir` を直接呼んでいる。blob の unlink もシームを通らない（旧実装から） | `objects::sync_dir` / `fault::unlinked` に通し、crash と `Fault::PowerLoss` のスナップショットのテストを足す。修正前に PowerLoss で落ちることを先に確かめる |
| `BlobBackend` が `Catalog` に依存している | `src/storage/catalog/blob_backend.rs:199` の `publish_resumable(&Catalog, …)` | ディスクのパスの組を返すメソッドを backend に置き、`publish_staged` / `run_upload` は Catalog 側で呼ぶ |
| 保持期限のオーバーフローの分類 | 両方の公開経路で、`ObjectError::InvalidMetadata`（"custom metadata exceeds 8 KiB"、400）になる。壊れた保存値がクライアントのエラーに見える | `Corrupt` か専用のバリアントにする。API で設定できる期間では到達しない |

### 4.5 sync

| 項目 | 現状 | 方針 |
| --- | --- | --- |
| 停止理由・カテゴリ・状態が文字列 | `src/sync/mirror.rs:569-603` の `pause_reason` が `&'static str` を返し、`"observer-failure"` だけ kebab-case になっている。`src/server/admin/sync.rs:385` で文字列を再解釈して `"Paused"` に丸めている。`src/sync/mirror/wait.rs:98` で `state != "Ready"` と比べている。`findings.rs` の `(category, reason)` も文字列の組 | `enum PauseReason` / `enum FindingCategory` を作り、`ToSql` / `FromSql` を持たせる。`FromSql` では、DB に保存済みの綴りを受け付ける |
| 原因のでっち上げ | `src/sync/mirror.rs:684` の `Stop::IoFailure => AdminError::Io(io::ErrorKind::Other.into())` | 上の enum を入れれば、`Stop` をそのまま運べる |
| journal の再生の方針が不一致 | `src/sync/mirror.rs:531` で、不正なバケット名はスキップするのに、不正なオブジェクト名ではミラーを停止する | 方針を決めて揃える（finding として記録してスキップするか、両方とも Corrupt として停止するか） |
| `ResolveTarget` の Option | `src/storage/catalog/sync_state/conflicts/resolve.rs:22,28` は、本番では常に `Some` | 入力の型を分ける。`ResolveRequest` が保存されている可能性があるので、既存のデータとの互換を確かめてから行う |

### 4.6 import と source_tree

| 項目 | 現状 | 方針 |
| --- | --- | --- |
| 配列の添字の意味とカウンタ | `src/import/reader.rs` で、`times: [i64; 2]` を添字で使っている（0 が created、1 が updated）。`objects` を `scan()` で増やし、`entry()` で減らしている | 名前付きのフィールドにする。オブジェクトとして数えるかの判定を 1 つの関数にまとめる |
| corrupt のフィールド名が文字列 | `ImportFailure::corrupt("name", …)` が 29 か所ある。serde のエラーを `format!` で文字列化していて、`source()` がない | field を enum に寄せ、原因を保持する |
| `Fingerprint::check` の Result の誤用 | `src/import/reader.rs` | `changed_fields()` を作り、`check` はそのラッパーにする |
| watch のカウンタの重複 | `src/source_tree/watch.rs:28,255` の `count` が、`files.len()` と常に同じ | `count` を消す。起きない失敗のエラーも消す |

### 4.7 テスト

| 項目 | 現状 | 方針 |
| --- | --- | --- |
| スキーマ移行のテストの定型処理 | `src/storage/catalog_tests.rs`、`src/storage/object_tests.rs` で、close → `Connection::open` → 戻す → 再オープン、の流れが繰り返されている | `migrated(dir, record, downgrade)` と `assert_current_schema` にまとめる |
| fixture の重複 | `BucketRecord` の全フィールドのリテラル（resumable 系 4 件）、`Publication` のリテラル（28 件）、アップロード本文の channel（約 20 件）、全量読み出しのループ（9 件）、手動の時計（6 種類: `src/storage/resumable_tests.rs:14` ほか） | `test_fixtures::{seed_bucket, publication, body, read_all}`、`clock::ManualClock` にまとめる |
| crash の種別が bool | `for power_loss in [false, true]` が 10 か所ある | `enum Crash { Abort, PowerLoss }` と `Crash::ALL` を `fault.rs` に置く。電源断のケースは消さない |
| storage のテストヘルパーに `#[track_caller]` がない | 0 件 | アサーション系のヘルパーに付ける |
| writeback / conflicts の fixture | 再起動と分解（drop して close）の手書きが重複している | `Fixture::restart` / `shut_down` を作る。journal を再び有効にするかどうかは、引数で明示する |
| 包含されているテスト | `catalog_tests.rs` の `timestamp_version_fourteen_reopen_preserves_timestamps` | マイクロ秒の値と `user_version` の確認を、`create_get_delete_and_restart_preserve_every_field` に移してから削除する |
| 管理 API の sync のテスト | `src/server/admin/sync/tests.rs`（1269 行）で、保護された workspace を arm する準備が 2 か所に重複している。`unwrap_err().status` が 28 か所ある | `Fixture::armed_workspace`、`status_of`、表駆動化、`enum Tree` |
| xml / server の Fixture | `xml/post/failure_tests.rs` と `xml/put/streaming/tests.rs` の `Fixture::new` がほぼ同じ | 共通化する。`State::for_test` に寄せる |
| json の単体テスト | `src/json/tests.rs` の Python 風の真偽値のループが重複している。`json/page_token.rs` のトークンの偽造が 3 か所ある | ヘルパーにまとめる。`token::seal` を使う |
| 固定名の一時パス | `src/import/filesystem.rs:389`、`src/config_input.rs:1138`、`src/server/copy.rs:182`、`src/server/gzip.rs:221`、`src/storage/catalog.rs:2195,2395,2434,2538,2604`、`src/storage/catalog/rewrite.rs:769` | `TestDir` にする。乱数のサフィックスと片付けがあるものは別扱いでよい |
| bool のモード引数 | `Harness::start(max, strict: bool, …)`（92 か所）、`memory_tests::setup(budget, versioning: bool)` など | 2 値の enum にする |

### 4.8 xtask（開発ツール。production lint の対象外）

- `type Result<T> = std::result::Result<T, String>` が 15 モジュールにある。`map_err(|e| e.to_string())` が約 68 か所ある。xtask 共通の `Error` と `trait Context` で、原因を保持する。
- `xtask/src/differential/case.rs` の `Step` は、gRPC なのに path を持つ、といった不正な状態を表現できる。`enum Target { Http { method, path, query }, Grpc { rpc } }` にする。
- curl のラッパーが 2 つある（`xtask/src/dependencies.rs`、`xtask/src/differential/transport.rs`）。`"8388608"` を直書きしていて、`MAX_RESPONSE` と二重管理になっている。モードを bool で渡している。

## 5. 判断が必要なもの

| 項目 | 現状 | 選択肢 |
| --- | --- | --- |
| 取り込み identity のハッシュ必須チェック | 「MD5 / CRC32C 必須・Replace 限定」のガードが、`#[cfg(test)]` のラッパー（`src/storage/catalog/admin/imported_identity.rs:85-110`）にしかない。本番の `publish_with_identity`（`src/storage/catalog/admin.rs:706`。fixture の管理 API から呼ばれる）にはない。テストは、テスト用のコードを検証しているだけ | (a) ガードを本番に移す（ハッシュのない fixture が 400 になる）。(b) fixture ではハッシュを任意とし、ラッパーと 2 本のテストを消して doc を直す |
| seed のメタデータの上限の測り方 | `src/storage/catalog/admin/publication/reseed.rs:81-97` で、8 MiB の上限を `{value:?}` の Debug 出力の長さで測っている | 実データのバイト数で測る。上限の境界が少し変わるので、変化として明記する |
| gRPC の範囲外の整数 | `src/grpc/convert.rs` で、`u64` から `i64` への変換を、旧実装の `as` と同じ折り返しに保っている。範囲外の値はワイヤーで符号が反転する | 飽和させるか、エラーにするか。振る舞いの変化になる |
| ACL ロール・ストレージクラスの未知の値 | 4.1 を参照 | `Corrupt` にするか、保持するか |
| 診断ログの形 | 4.2 を参照 | `message` を残して互換にするか、構造化するか |

## 6. 見送ったもの（理由付き）

| 項目 | 理由 |
| --- | --- |
| `BucketRecord.lifecycle` の型付け | GCS のエラー文言と、未知のフィールドの往復を保つ必要がある。`Value` のほうが正直な表現 |
| `Option<Option<T>>` を `Patch<T>` にする | PATCH の意味として serde の定番の書き方。変えても安全性は上がらない |
| json の `pub` の削減 | `tests/` が公開 API として使っている。効果が小さい |
| テストを `-> Result` と `?` の形にする | 失敗した行が出なくなる。リポジトリは意図して unwrap を選んでいる |
| rstest の導入 | 既存の `for` ループの表駆動と表現力が同じ |
| HELP の生成 | 空白・折り返し・値の表記が不揃いで、バイト互換のリスクが効果を上回る。SHA-256 のテストで固定している |
| `Setting` のカテゴリの enum と、`Settings` のキーの型付け | すべての validator と診断の順序（キーの辞書順）に影響が広がる。行う場合は、`Ord` を `as_str()` の順に合わせること |
| パスの重なり判定の一本化 | 存在しないパスや symlink の扱いが、実装ごとに違う。挙動が変わる |
| gRPC のパッチの組み立てから serde_json をなくすこと | JSON のパーサが担っている検証の切り出しが前提になる（`json!` が約 56 か所）。v1 の一覧は今回切り離した |
| v1 / v2 の NotEmpty の文言の統一 | 公式資料で根拠を確認できない。両方を固定するテストを置いた |
| `CorsWarnings` と forwarded の除外集合の二重管理 | `debug_assert` と `From<JsonError>` のアームで、同じステータスの集合を持っている。アームを足すときは両方を揃える（参考事項） |
| `GCS_MIMIC_` の接頭辞の重複（`src/config_input.rs:245,273`） | 2 か所だけで、効果が小さい |

## 7. 推奨の検証

各スライスで、次を実行してください。

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask lint
bash scripts/check-cross.sh
cargo test --workspace --lib --locked
cargo test --locked --test <対象の it_* バイナリ>
```

最終ソースで `just check` を 1 回実行します。保存形式やワイヤーに触れるスライスでは、SDK のスイート（`just check-sdk`）を、少なくとも Go と Ruby で実行してください。durability の経路に触れるスライスでは、AGENTS.md の fault のシームのルールに従い、crash と `Fault::PowerLoss` のテストを足します。

## 8. 着手順の提案

1. ガードレールの穴（`spawn_blocking`、`#[allow]`）: 小さく、確実に終わる。
2. 管理用の cleanup の fault のシーム: durability の穴なので早めに直す。
3. ストレージ障害の文字列判定の型付け: 本質的な改善。storage・json・gRPC にまたがるので、1 つのスライスで行う。
4. sync の停止理由の enum 化と、原因のでっち上げの解消。
5. HTTP の残り（X-Goog-Hash、時刻、各種の重複、ヘッダ名の定数）。
6. storage のテストの fixture 化と、そのほかのテストの整理。
7. AclRole / StorageClass / Hold（5 の判断のあと）。
8. import / source_tree / xtask。
