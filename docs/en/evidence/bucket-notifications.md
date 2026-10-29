# Bucket notification configuration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [bucket-notifications.md](../slices/bucket-notifications.md).
Rows: JSON-NOTIFICATIONS-001..004 (PARTIAL) and AUDIT-021 (IMPLEMENTED).
All runs used the pinned Rust 1.99.1 toolchain, Go 1.27.3 with
`cloud.google.com/go/storage` v1.70.0, and Ruby 4.0.9 with
`google-cloud-storage` 1.62.1, over plain HTTP on loopback. No Google Cloud or
metadata-server endpoint was contacted; the emulator publishes no Pub/Sub
messages.

## Results (2026-10-29, final source)

- `cargo fmt --all --check`: PASS.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: PASS.
- `cargo test --locked --offline --lib notification`: PASS, 12 tests (4 domain validation, 4 JSON adapter, 4 catalog persistence/migration/corruption).
- `cargo test --locked --offline --test notifications_http`: PASS, 4 tests (round trip and restart, invalid input without mutation, strict permissions, deprecated watch/channel routes).
- `bash scripts/test-rust.sh`: PASS, 46 test binaries plus doctests, 1129 tests, 0 failures. Existing schema-version assertions moved from 27 to 28 and the future-schema fixtures from 28 to 29; downgrade helpers drop the two new tables. The unsupported-route inventory tests now treat the four notification methods as implemented.
- `cargo xtask compatibility check`: PASS, 606 feature rows; generated English and Japanese matrices are current.
- `just check-sdk` (HTTP): PASS. Go vet and the full Go suite pass, including `TestBucketNotifications`, `TestStrictAuthBucketNotifications` and `TestUnsupportedFailsFast` without the former notifications case. The Ruby permissive and strict suites both print `ruby-bucket-notifications passed` and `ruby-unsupported-fails-fast passed`.
- `bash sdk-scorecard/run.sh --out "$TMPDIR/scorecard-notifications" gcs-mimic`: PASS. `bucket.notifications` passes for Go and Ruby. Compared with `sdk-scorecard/results/2026-10-29/gcs-mimic.*.jsonl`, no previously passing case regressed; Go 45 to 46 of 57 and Ruby 40 to 41 of 51 cases pass (score 80.5). Scorecard results were not committed.
- `npx --yes markdownlint-cli2` on the changed documents: PASS.

## Not performed

Real-cloud differential runs, HTTPS SDK runs and container builds were not
performed. Topic existence, publisher permission and event delivery remain
unimplemented and are recorded as PENDING acceptance cases.
