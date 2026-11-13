# Bucket notification configuration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [bucket-notifications.md](../slices/bucket-notifications.md).
Rows: JSON-NOTIFICATIONS-001, -002 and -004 (IMPLEMENTED since 2026-11-13),
JSON-NOTIFICATIONS-003 (PARTIAL) and AUDIT-021 (IMPLEMENTED).
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

## Topic existence check against a configured Pub/Sub emulator

With `pubsub_emulator_host` set, creating a configuration asks the emulator
whether the topic exists (`GET /v1/projects/P/topics/T`, five-second timeout).
The cases below run against an in-test stand-in emulator only; no real Pub/Sub
emulator or Google service was contacted.

- `cargo test --test notifications_http`: PASS, 8 tests and 1 opt-in ignored.
  `creating_a_config_checks_the_topic_against_the_configured_emulator` covers
  an existing topic, a missing topic (recorded 400), the validation order (an
  invalid `payload_format` and a missing `topic` never reach the emulator; an
  invalid `event_types` is reported only after the topic is confirmed), a 500
  answer and a refused connection (both 503, nothing created).
  `a_missing_bucket_is_not_found_without_asking_the_emulator` shows a missing
  bucket is a 404 and sends no lookup.
- `cargo test --lib -- notification`: PASS, 37 tests, including
  `topic_lookup_separates_missing_from_unconfirmed` (200, 404, 500, 307,
  refused connection) and `topic_lookup_without_a_usable_answer_is_unreachable`
  (no reply before an injected timeout, a non-HTTP reply, a body past the
  bounded read), and the 503 mapping and log-cause tests in
  `json::notifications`.
- Unrecorded: the recording sent only the full `//pubsub.googleapis.com/...`
  topic form. Naming the normalized topic in the error for a short
  `projects/P/topics/T` request is a guess.

## Results (2026-11-13, get, list and delete completion)

- `cargo test --locked --offline --lib`: PASS, 2093 tests, including
  `storage::catalog::notifications::tests::insert_and_delete_advance_the_bucket_updated_time_with_its_metageneration`,
  which sets a manual clock and checks that an insert and a delete each move
  the metageneration and `updated` to that instant while a delete of an absent
  ID changes neither.
- `cargo test --locked --offline --test it_json --test it_admin`: PASS, 137 and
  79 tests.
- `cargo xtask differential run --reference compatibility/differential/reference/gcs.json --fail-on-diff --fail-on-note`:
  PASS, all 102 cases match, including `bkt-notifications` and
  `bkt-edges-notification-ids`. The second case was recorded against Cloud
  Storage on 2026-11-13 without a topic or configuration; see
  [provider probes](provider-differential-gap-probes.md).
- `env -u GCSM_GO_TEST_RUN bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-ruby`:
  PASS, 92 Go tests including `TestBucketNotifications` and
  `TestStrictAuthBucketNotifications`, and both Ruby runs print
  `ruby-bucket-notifications passed`.

## Not performed

HTTPS SDK runs and container builds were not performed. The real-cloud
recordings created no topic or configuration, so a successful insert, its
etag and its `updated` effect were not recorded. Publisher permission is not modeled, and topic existence is checked
only when a local emulator is configured; without one the topic is accepted
unchecked.
