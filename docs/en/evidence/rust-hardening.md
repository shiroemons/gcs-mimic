# Verification: internal Rust hardening before 1.0.0

This records the checks run for [DESIGN-105](../slices/rust-hardening.md), on
macOS arm64 with Rust 1.100.0 from `rust-toolchain.toml`. Paths are relative to
the repository root. Each slice adds the suites that cover it; the final source
section is filled in once, on the integrated source.

## Safety nets

What changed:

- Error goldens. One file per adapter pins every answer:
  - `src/json/error/golden_tests/responses.txt`: 243 cases, 661 renderings.
    Each rendering has the status, the headers in the order sent and the
    exact body bytes.
  - `src/xml/error_mapping/golden_tests/responses.txt`: 219 cases, 811
    renderings, with the same detail.
  - `src/grpc/v2/status/golden_tests/statuses.txt`: 211 cases, 799
    renderings. Each has the code, the message and the details.
  - The case lists match every variant without a wildcard, so a new variant
    fails until it has a case.
  - `GCSM_UPDATE_GOLDENS=1` rewrites a file that differs and still fails the
    run.
  - The fake-gcs-server bucket route is rendered through the function the
    route itself answers with.
- Persisted-format goldens pin these through the code that writes them:
  - the stored JSON and gRPC completion text;
  - the upload replay completion document;
  - the replay fingerprints for a custom time and a copy;
  - the default bucket record;
  - the bucket an import publishes.
- Layer and adapter rules:
  - `text::adapter_dependency` finds nothing.
  - `text::layer_dependency` starts at 108 references in 58 files, ratcheted
    in `xtask/lint-baseline.txt`.

| Check | Command | Result |
| --- | --- | --- |
| Workspace suite on the integrated source | `cargo test --locked --workspace --no-fail-fast` | 3859 passed, 0 failed, 1 ignored in 17 test binaries; the library's 2443 and `xtask`'s 193 passed |
| Golden stability | `cargo test --locked --lib -- golden_tests:: grpc::v2::stored::tests::completion_text`, five runs in a row | 15 passed in each run |
| Layer baseline | `cargo xtask lint --baseline`, then `git diff -- xtask/` | No difference from the committed baseline |
| Production lint gate | `cargo xtask lint` | 0 new production violations; 108 ratcheted findings |
| Clippy | `cargo clippy --locked --all-targets` | No warnings |

The goldens pin today's behavior, including answers that look wrong; the
design lists them under [Behavior decisions](../slices/rust-hardening.md#behavior-decisions).
Protobuf maps encode in `HashMap` order. So the gRPC completion golden uses a
single metadata entry, and the gRPC golden re-encodes `ErrorInfo` details in
key order after checking that they decode to the same value. The protobuf crate
slice removes the need for both.

## Writer-thread I/O

What changed:

- The XML part and multipart completion paths sync their staging entry on the
  blocking pool before the job that commits the row, instead of inside that
  job.
- A source batch that fails part way returns the pins it already made, and the
  request unlinks them on the blocking pool.
- A reply whose caller was cancelled is dropped on the blocking pool of the
  caller's runtime.
- The read pool takes an idle reader in its own statement, so the idle list is
  not locked while a new reader thread starts.
- In test builds, every durability seam that can run synchronously records its
  caller when it runs on a catalog executor thread, and `Catalog::close()`
  fails when its data directory recorded one outside the exclusive reclaim
  allowance that reset and reseed hold.

| Check | Command | Result |
| --- | --- | --- |
| XML tests, with the two new sync steps in the multipart failed-step, crash and power-loss loops | `cargo test --locked --lib -- storage::catalog::xml_multipart xml` | 130 passed |
| Executor tests | `cargo test --locked --lib -- storage::catalog::executor` | 4 passed |
| Assertion and source-batch tests | `cargo test --locked --lib -- storage::catalog::tests::close_fails_naming storage::catalog::tests::a_job_under storage::catalog::tests::the_allowance_ends storage::catalog::derived:: storage::resumable_http_tests::csek grpc::v2::update_copy::object_keys` | 51 passed |
| Workspace suite, every library test that closes a catalog checking the assertion | `cargo test --locked --workspace --no-fail-fast` | 3842 passed, 0 failed, 1 ignored in 17 test binaries; the library's own 2434 passed |
| Production lint gate | `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| Clippy | `cargo clippy --locked --all-targets` | No warnings |

The targeted runs came first; the workspace suite, Clippy and the lint gate ran
on the committed source.

The first library run with the assertion failed three tests, all at
`src/storage/catalog/blob_backend.rs` `remove_pin`: a compose or copy whose
later source failed dropped the pins of the earlier sources inside the writer
job. That run is why the source batch now returns its pins.

Limits:

- Asynchronous seams, such as `StagedFile`, are not recorded: an executor job is
  a synchronous closure and cannot await them.
- Integration tests link the library built without `cfg(test)`, so only the
  library's unit tests check the assertion.

## Writer settings

What changed

- The disk catalog writer keeps prepared statement plans across changes in bound values.
- The writer documents why `wal_autocheckpoint = 256` triggers a passive checkpoint sooner,
  reducing the work a committing request can inherit. A long read can still hold the WAL open.
- The persistence design records the writer's in-memory temporary store and names
  `CATALOG_SCHEMA_VERSION` as the schema version source.

| Check | Command | Result |
| --- | --- | --- |
| Workspace suite on the first-phase source, 2026-11-26 | `cargo test --locked --workspace --no-fail-fast` | 3,860 passed, 0 failed, 1 ignored, and no compiler warnings across 14 test binaries and two doctest crates. The library passed 2,444 and `xtask` passed 193 unit tests. The error and persisted-format goldens passed. |

Limits

- The ignored test requires an explicitly started local Pub/Sub emulator. A sandboxed
  loopback probe returned `Operation not permitted` before the workspace suite and
  `just check` ran with loopback access.
- Prepared write transactions and the reader statement cache increase belong to later
  slices. The suite verifies behavior. Query-planning performance was unmeasured.

## Lint quick wins

What changed

- The production gate checks `clippy::significant_drop_in_scrutinee`,
  `clippy::or_fun_call` and `clippy::return_and_then`.
- Workspace lints deny `clippy::ignore_without_reason` and `cargo::unused_dependencies`.
- Drop guards that callers must keep alive are marked `#[must_use]`.
- Gate tests keep the Rules table in sync with the production rules and workspace lint levels.

| Check | Command | Result |
| --- | --- | --- |
| Shared checks on the first-phase source, 2026-11-26 | `just check` | Passed, including all 14 test binaries and doctests (3,860 passed, 0 failed, 1 ignored). |
| Host and release-target Clippy within the shared checks | `just check` | No warnings on `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` or `x86_64-unknown-linux-musl`. The host production gate found 0 new violations and 108 ratcheted findings. Each cross-target gate found 0 new Clippy violations and 0 ratcheted findings. |
| Rules table and workspace lint levels | `cargo test --locked --workspace --no-fail-fast` | Both `xtask` tests passed within the workspace suite. |

Limits

- `large_futures`, the additional `disallowed-methods` rules and parser property tests
  belong to later slices. The layer gate still has a per-file ratchet.

## Small fixes

What changed

- HTTP/1 returns 431 for a request head still incomplete at its read buffer threshold.
  The 40 KiB test pins the rejection and the design explains the possible overshoot.
  The `MAX_QUERY_BYTES` comment records its use by Storage v1 gRPC.
- The shared diagnostic helper defers building disabled debug events for connections,
  task cancellations, authorization denials and catalog reader fallback.
- The public contract review links to the catalog upgrade guide for schema history.

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --check` | Passed. |
| Header rejection and diagnostic threshold | `cargo test --locked --workspace --no-fail-fast` | Both tests passed within the workspace suite. |
| Ledger and generated matrices | `cargo xtask compatibility check` | Passed for 611 feature rows, 401 parameter locations and 357 schema fields. |
| Changed prose | `npx --yes markdownlint-cli2 AGENTS.md docs/en/design.md docs/en/development.md docs/en/evidence/rust-hardening.md docs/en/native-binary.md docs/en/public-contract-review.md docs/en/slices/rust-hardening.md docs/en/slices/xml-put-object.md docs/ja/development.md` | Nine files, zero issues. |
| Generated views | `npx --yes markdownlint-cli2 --config docs/generated-views.markdownlint-cli2.jsonc docs/en/feature-matrix.md docs/ja/feature-matrix.md` | Two files, zero issues. |
| Documentation checks | `bash scripts/check-docs.sh` | Passed, including four CLI help tests, the environment namespace, generated matrices and release documentation checks. |

Limits

- The HTTP/1 test covers rejection at 40 KiB. Hyper checks the 16 KiB threshold while
  the request head is incomplete, so a read that completes it can exceed the threshold.

## HTTP/1 read buffer at 64 KiB

On 2026-11-26, `cargo test --locked --lib -- server::protocol_tests::http1_enforces_the_request_head_buffer_limit`
passed one test on the 64 KiB source. The HTTP/1 service accepted a request carrying
a 40 KiB header value with status 200 and rejected a 160 KiB value with status 431.

## Error model and wire goldens, 2026-11-27

The workspace run at `328b3069f` passed 4,179 tests with
`cargo test --locked --workspace --no-fail-fast`. The one ignored test
requires an explicitly started local Pub/Sub emulator. The JSON, XML and
Storage v2 error fixtures, stored completion bytes, replay fingerprints and
bucket defaults passed with no changes to their expected bytes.

The error tests check the concrete causes of storage and adapter failures,
the text that the CLI and import journal expose, and the documented cases
where callers can print and inspect the same cause. The verification entry
for this run lists the test IDs observed in its output.

With the source-format lint using reasoned suppressions, a separate workspace
run passed 4,184 tests, with the same Pub/Sub case ignored. The gate counted
four TLS suppressions and zero new violations. Tests confirmed that changing
a source-printing format to transparent makes its suppression stale. The CLI
save and bucket-name errors exposed the same concrete causes they formatted,
and gRPC conversion preserved the underlying storage source.

## Public facade, 2026-11-28

`just check` passed on the source that keeps every library module private
except `app` and `testing`. Its Rust lane passed 4,189 tests with the same
Pub/Sub case ignored. The production gate counted zero new violations, 70
ratcheted findings and nine reasoned suppressions on the host, and zero new
Clippy violations for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and
`x86_64-unknown-linux-musl`. The ledger check validated 611 feature rows.

The library-surface test confirmed that `src/lib.rs` declares only `app` and
`testing` as public. The compose parsing tests passed from their new place in
the library, and the source tree predicates passed in their own library test
beside the import test they came from. `DurationError::Number` reported its
own text with the `ParseIntError` as its source.

An earlier `just check` on the same source failed one test,
`obj_http_trickled_resource_body_renews_body_inactivity`, with a 503 after the
500 ms request deadline, while 4,184 others passed. The test passed five runs
alone and a run of its whole `it_protocol` binary, and the repeated
`just check` passed it.

## Foundation moves, 2026-11-28

`just check` passed on the source that parses RFC 3339 text in `time_format`
instead of `import::codec`. Its Rust lane passed 4,191 tests with the same
Pub/Sub case ignored. The production gate counted zero new violations, 69
ratcheted findings and nine reasoned suppressions on the host, after the
`src/object.rs` layer finding left the baseline, and zero new Clippy
violations for the three other targets. The ledger check validated 611
feature rows.

The parser's tests passed from `time_format`. The import codec tests that
parse bucket, retention and custom times passed with import's own timestamp
failures as codec error kinds, and the platform range failure kept its text
and cause. Two new codec tests confirmed that the Go zero time still reads as
unset and that names still escape as Go's `PathEscape` does. A Clippy probe
added to the parser module and then removed confirmed that the module rejects
`unwrap` and `expect`.

## Model value types, 2026-11-28

`just check` passed on the source that moves `CompatProfile` to `profile`,
`TrustedCaller` to `auth`, the DNS name and reserved path checks to `url`, and
`AdminGrant` with its `Origin` to `bucket`, and that checks project IDs with the
`auth` validators instead of a JSON wrapper. Its Rust lane passed
4,191 tests with the same Pub/Sub case ignored. The production gate counted
zero new violations, 57 ratcheted findings and nine reasoned suppressions on
the host, after twelve layer findings in `bucket`, `bucket::name`,
`bucket::resource`, `auth::built_in`, `auth::policy_input`, `url` and
`storage::catalog::policies` left the baseline, and zero new Clippy
violations for the three other targets. The ledger check validated 611
feature rows.

The moved `validate_dns_name` and `reserved_path` bodies were identical to
the originals. The `InvalidDnsName` text test passed from `url`, and the
project identifier test passed from the policy input test module. The
`storage::admin` compile-fail doctests passed. A probe that removed the
reasoned exception on the one function that mints administrative grants made
`cargo xtask lint` report its `AdminGrant::new` call as a disallowed method,
so the `clippy.toml` entry resolves the crate-local path.

## JSON-owned rules, 2026-11-28

`just check` passed on the source that moves `Projection` to `acl`, the
derived component count to `object::derived`, and the virtual-host
authority helpers to `config::domains`, and that converts bucket
initialization entries through `bucket::resource::parse_insert` instead of
the JSON body parser. Its Rust lane passed 4,192 tests with the same Pub/Sub
case ignored; the new test is the `object::derived` component count test.
The production gate counted zero new violations, 45 ratcheted findings and
nine reasoned suppressions on the host, after twelve layer findings in
`storage::catalog::resumable`, its options, `storage::catalog::rewrite`,
`bucket_init`, `config::draft`, `config::runtime` and
`notification::event` left the baseline, and zero new Clippy violations for
the three other targets. The ledger check validated 611 feature rows.

The moved bodies of `bucket_for_authority`, `split_virtual_host`,
`virtual_host_api_authority` and `request_authority` were identical to the
originals. The initializer tests and the XML authentication suite passed
with the conversion and the authority helpers in their new places. No test
reaches the defensive size branch in `bucket_init::records()`, so its
equivalence with the former `serde_json::to_vec` measurement rests on the
entry's field types and on `preserve_order`.

## Notification renderer, 2026-11-28

`just check` passed on the source in which `notification::event` renders a
`JSON_API_V1` payload through `notification::JsonApiObject`, which the JSON
adapter implements for `ObjectRecord` with the former `noAcl` rendering and
empty endpoint, and encodes it with the foundation `base64::encode`. Its Rust
lane passed 4,192 tests with the same Pub/Sub case ignored; no test was added,
because the payload is unchanged. The payload field order test in
`json::object_tests` and the notification outbox tests, which build their
expected data through `json::object_resource` and `json::base64`, passed
through the trait. The production gate counted zero new violations, 43
ratcheted findings and nine reasoned suppressions on the host, after the two
`notification::event` layer findings left the baseline, and zero new Clippy
violations for the three other targets. The ledger check validated 611
feature rows.

## `auth` split and `HmacKeys`, 2026-11-28

`just check` passed on the source in which `auth::signature` looks up HMAC
keys through `HmacKeys`, which `Catalog` implements, and `config::auth` reads
the identity document, holds both forms to 16 MiB and names the setting in
`IdentityError`, so `Auth::load` parses bytes and `RegistryError` holds only
what parsing rejects. `AuthProfile<D>` and `Verification` live in
`auth::profile`, and `src/auth/mod.rs` is split into `permission`, `choices`,
`error`, `identity`, `registry`, `authenticate` and `auth/tests.rs`, whose
re-exports are listed except the permission constants that a macro defines.
A sorted-line comparison of the former `mod.rs` with
the split files showed only module docs, declarations, re-exports, imports
and one rewrapped signature. Its Rust lane passed 4,193 tests with the same
Pub/Sub case ignored, one more than before: `config::auth::error_text_and_sources`
checks the text and source of each `IdentityError` variant, which
`auth::error_text_and_sources` and the `RegistryError` text test no longer
cover. `config::auth::tests::registry_file_read_is_bounded`, moved from
`auth::tests`, passed with its assertions unchanged. The invalid registry
documents still fail in `auth` without echoing tokens, and the configuration
load test names the setting for document, entry, syntax, public key and
project failures. The production gate counted zero new violations, 39
ratcheted findings and nine reasoned suppressions on the host, after the
layer findings of `src/auth/mod.rs` and `src/auth/signature/{policy,streaming,verify}.rs`
left the baseline, and zero new Clippy violations for the three other
targets. The ledger check validated 611 feature rows.
