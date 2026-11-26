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
