# Environment variable namespaces

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-11-11 for DESIGN-064. The server reads `GCS_MIMIC_*`;
non-setting client, harness and script variables use `GCSM_*`. No setting
name, feature status or fidelity changed. The rename list was derived by
comparing tracked `GCS_MIMIC_[A-Z0-9_]+` tokens with `src/config/setting.rs`:
48 non-setting names moved, including the seven public names and 41 repository
test/harness names. The signed-browser harness uses `GCSM_BINARY` rather than
introducing a second `GCSM_` name for the binary. The four deliberate
unknown-setting inputs remain unchanged.

The [migration table](../releases/0.18.0-contract-changes.md#environment-variable-names)
names the seven public replacements. The documentation guard derives settings
from `Setting` and permits those seven deliberately rejected old names,
the four existing unknown-setting inputs and the explicitly baselined historical
records. A temporary unregistered token was
rejected with its file and line; `scripts/test-check.sh` preserves that negative
case with a mocked tracked-file scan.

| Command | Result |
| --- | --- |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo test --locked --offline --test it_process variables -- --nocapture` | PASS: 2 tests; all seven new variables coexist with startup and all seven old names fail individually with exact environment diagnostic keys |
| `cargo test --locked --offline --test it_process non_environment_names_keep_unknown_setting_advice` | PASS: 1 test; all seven configuration keys and all six unsupported CLI flags retain unknown-setting advice |
| `cargo test --locked --offline --test it_sync live_cli_ -- --nocapture` | PASS: 10 tests; new token authentication, token-file precedence and old-token rejection through the real process |
| `cargo xtask lint` | PASS: zero new production violations and zero ratcheted findings |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `go test -mod=readonly ./...` in `go/` | PASS: subprocess package and SDK guard; new binary selection, old-variable refusal and isolated child environments |
| `go vet -mod=readonly ./...` in `go/` | PASS |
| `go test -mod=readonly ./...` in `go/sourceexport/` | PASS |
| `bash scripts/check-docs.sh` | PASS |
| `bash scripts/test-check.sh` | PASS, including namespace drift rejection |
| `bash scripts/test-pre-push.sh` | PASS |
| `cargo test --workspace --locked --offline` | One concurrent sync test failed: 31 passed and 1 failed in `it_sync`; all preceding binaries passed |
| `RUST_TEST_THREADS=8 cargo test --workspace --locked --offline` after the diagnostic changes | Library: 2041 passed, 1 existing 100ms batch-deadline test failed; exit 101 |
| `cargo test --locked --offline --lib server::batch::tests::a_part_past_the_request_limit_answers_alone_and_later_parts_still_run -- --exact --test-threads=1` | PASS: 1 test, 2.10 seconds |
| `cargo test --locked --offline --bin gcs-mimic` after the diagnostic changes | PASS: 32 tests |
| `RUST_TEST_THREADS=8 cargo test --workspace --locked --offline --test '*'` after the diagnostic changes | PASS: 1063 integration tests across 11 binaries; 1 existing opted-in Pub/Sub test ignored; exit 0 |
| `cargo test --locked --offline --test it_sync live_cli_inspects_stopped_sparse_recovery_and_requires_exact_confirmed_discard -- --nocapture --test-threads=1` | PASS: 1 test, 8.79 seconds |
| `cargo test --locked --offline --test it_xml_auth` | PASS: 317 tests, completing the binary not reached by the full run |
| `cargo test --locked --offline --package xtask` | 220 library tests passed; the local Pub/Sub listener test hit sandbox `PermissionDenied` |
| `cargo test --locked --offline --package xtask accept_failure_does_not_stop_the_next_lookup` | PASS with local listener permission |
| `cargo test --locked --offline --package xtask --test compatibility --test dependencies --test differential` | PASS: 11, 3 and 1 tests |
| `cargo test --locked --offline --package grpc-codegen` | PASS: 1 test |
| `cargo test --workspace --locked --offline --doc` | PASS: 4 server doctests; xtask has none |
| `bash scripts/check-markdown.sh` and explicit lint of this evidence file | PASS: zero Markdown issues |

One shared enum defines the seven suffixes, their purposes and server alternatives.
Server and sync diagnostics use its same repair text. The process checks cover
individual and simultaneous old variables, including all seven diagnostic keys
and server alternatives, and check both readiness stdout and stderr for client
variable names or secret values.

The sync process test rejects old-only, old-plus-new, old-plus-token-file and
old-plus-empty-new inputs; the last also verifies that an empty old value is
still rejected. An empty new variable without an old one names `GCSM_SYNC_TOKEN`
in its error rather than calling it a token file. The existing token-file
precedence test still passes when the old variable is absent.

The concurrent failure was:

```text
---- it_sync::sync_cli::workspace::live_cli_inspects_stopped_sparse_recovery_and_requires_exact_confirmed_discard stdout ----

thread 'it_sync::sync_cli::workspace::live_cli_inspects_stopped_sparse_recovery_and_requires_exact_confirmed_discard' (75665128) panicked at tests/it_sync/sync_cli/workspace.rs:99:5:
assertion `left == right` failed
  left: String("Timeout")
 right: "Paused"
```

The same test passed in the earlier ten-test CLI run and in the isolated rerun.
The final-source library run later failed in an unchanged batch test:

```text
thread 'server::batch::tests::a_part_past_the_request_limit_answers_alone_and_later_parts_still_run' (76040235) panicked at src/server/batch.rs:559:9:
assertion failed: next_text(&mut body).await.contains("204")
```

Its isolated rerun passed. No assertion or deadline was changed. A complete
concurrent green run remains a final-integration check.

After rebasing onto the ready-event directory changes, `cargo check --workspace
--locked --offline` and `cargo xtask lint` passed, and
`RUST_TEST_THREADS=8 cargo test --locked --offline --test it_process` passed all
90 process tests. `bash scripts/check-docs.sh` passed the namespace guard and
607-row ledger check. `bash scripts/check-markdown.sh` found zero issues in
388 Markdown documents and two generated views. Regenerating both feature
matrices produced no changes. The shared release draft retains both the ready
event and environment-variable sections. The rebased
`cargo test --locked --offline --test it_sync live_cli_` also passed all ten tests.

The first sandboxed process run failed with `Cannot bind listener; check
--listen IP:PORT and local network permissions.` The same targeted tests passed
with local listener permission. No negative test was removed or weakened.

The SDK runners still remove inherited server settings and `PUBSUB_EMULATOR_HOST`,
because flags define isolated test scenarios. The Go facade still removes
inherited `GCS_MIMIC_*` settings because its `Options` configure each child;
its documented Pub/Sub environment input is preserved. Both keep `GCSM_*`.

## Compose rejection and shared binary selection

The signed-browser script and Go facade both use `GCSM_BINARY`; the former
`BIN` harness suffix adds no separate setting. Running
`GCSM_BINARY=target/debug/gcs-mimic bash scripts/signed-browser.sh prepare https://localhost:4443 signed-browser-prefix initial`
produced the expected signed-URL document, checked with `jq`, without a server.

The Compose files declare the three obsolete Compose names as environment keys
without values. According to the [Compose environment specification](https://docs.docker.com/reference/compose-file/services/#environment),
an unresolved key is removed from the container environment; a resolved key is
passed through. This supplies only the server's existing rejection path, never
an image, port or mount configuration value.

Docker Compose 5.6.1 `config --format json` assertions verified root, strict-local and
full-stack recipes with shell values and with an explicit throwaway `--env-file`:
old names are passed to the server, while unset old names remain unresolved.
The same assertions verified `GCSM_IMAGE` and `GCSM_PORT` select the requested
image and port. A real server invocation containing the three old names failed
with three replacement diagnostics. No image was built or pulled for these checks.

## Example checks

| Example | Check |
| --- | --- |
| Go | `go test -mod=readonly .` compiles; no unit tests are defined |
| Node.js | `npm ci --offline --ignore-scripts --no-audit --no-fund`, then `node --check main.mjs` |
| Python | `uv run --locked --offline python -m py_compile main.py` |
| Ruby | `ruby -c main.rb`; `bundle check` using the installed Ruby 4.0.9 and its Bundler |
| Java | `mvn -B -q -o compile`, dependency-tree generation and `cmp dependencies.lock target/dependencies.lock` |
| .NET | `dotnet restore --locked-mode --ignore-failed-sources`, then `dotnet build --no-restore --disable-build-servers` |
| Rust | `cargo check --locked --offline` and formatting check |
| PHP | `php -l /app/main.php` in the already-local `composer:2.10.3` image with no network; host PHP is unavailable |
| gcloud | `bash -n gcloud-example.sh` |
| Shell launchers | `bash -n` for every example's `run.sh` |
| Compose | `docker compose config --quiet` for root, root with TLS, strict-local and full-stack files; TLS uses a throwaway mount path |

Each runnable client was also invoked with only the old endpoint variable to
verify refusal before API calls. Java and .NET initially lacked cache write
permission and restored assets respectively; their final compile checks passed.
Complete example API workflows, SDK suites and container builds are reserved
for final integration and were not run here.

## Preserving historical records

Existing evidence, past release notes, historical slice verification sections,
the Go verification transcript, the archived scorecard log and planning/decision
records were restored from the integration branch. Current instructions remain
on the new names, including the runnable sections of `grpc-v2-foundation.md`
and `sdk-tests/go/verification.md`; historical verification commands and SDK
evidence in the ledger retain their original names. The ledger's current
acceptance descriptions and references to renamed Rust tests retain the current
names so they still identify the implemented checks. No classification remains
undecided.

`scripts/environment-history-baseline.txt` explicitly lists each historical file,
the original non-setting suffix and its maximum occurrence count. The guard
reports the preserved count, rejects an increased count and rejects an old harness
name in a new evidence file. It does not exempt an entire directory. A replacement
within the same file at the same count cannot be detected by this baseline;
line fingerprints would be needed to detect that case.

| Preservation check | Result |
| --- | --- |
| `bash -n scripts/check-docs.sh scripts/test-check.sh` | PASS |
| `bash scripts/test-check.sh` | PASS, including the existing historical budget, an excess occurrence and a new evidence file |
| `bash scripts/check-docs.sh` | PASS; 607 feature rows validated; 452 historical occurrences in 81 files and 103 file/suffix pairs preserved |
| `git diff --exit-code feat/public-contract -- docs/en/evidence ':(exclude)docs/en/evidence/environment-names.md'` | PASS; no historical evidence differences |
| `just check-markdown` | PASS with registry access: 388 Markdown files and 2 generated views, zero issues; the sandboxed attempt failed with npm `ENOTFOUND registry.npmjs.org` |

All original ledger command strings remain present, with only this slice's new
verification command added. Full Rust tests, SDK suites and container builds
were not repeated for this documentation and guard correction.
