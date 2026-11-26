# Verification: internal Rust hardening before 1.0.0

This records the checks run for [DESIGN-105](../slices/rust-hardening.md), on
macOS arm64 with Rust 1.100.0 from `rust-toolchain.toml`. Paths are relative to
the repository root. Each slice adds the suites that cover it; the final source
section is filled in once, on the integrated source.

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
