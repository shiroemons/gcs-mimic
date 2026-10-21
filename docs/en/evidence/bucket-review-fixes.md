# Bucket CRUD review remediation

Verified on 2026-10-21. No feature status was promoted. The canonical ledger and
both generated matrices were validated, regenerated and checked; rendering left
the matrices unchanged because their displayed status/scope did not change.

## Findings, fixes and commits

| Finding | Fix | Commit | Executed verification |
| --- | --- | --- | --- |
| M1 | Replace all 26 shortened/unrelated bucket implementation references with the actual 40-character implementation commits; verify existence and ancestry | `e43c94488ce3d320d07d7017a599710f2af77270` | `git cat-file -e '<sha>^{commit}'` and `git merge-base --is-ancestor <sha> HEAD` for every distinct non-null ledger SHA; compatibility validate/render/check |
| M2 | Zero selects the projection default (1000 noAcl / 200 full), including continuation; document proto3 rationale and unverified JSON behavior in the slice and both affected deviations | `78a75e46a23a0b2502be7121aabccbfc0e12e365`, `e43c94488ce3d320d07d7017a599710f2af77270` | `bkt_lst_001_max_results_defaults_bounds_and_clamping`, `bkt_http_sorted_scoped_pagination_and_limits`, `bkt_http_list_clamps_to_1000_with_lookahead` over 1001 buckets |
| M3 | Check successive `/v2`, `/v3`, etc. module paths; only 404/410 mean absent, while other failures return errors | `b141b3a416ef53feff174cd3b31d367437e32fd2` | `go_new_major_paths_are_checked_until_missing`, `go_missing_majors_and_proxy_failures_are_distinct`; online dependencies check exit 0 |
| L1 | Check version, emptiness and existing schema before changing journal mode | `78a75e46a23a0b2502be7121aabccbfc0e12e365` | `rejects_future_schema_unversioned_data_and_corruption`: future/unversioned/missing/incompatible schemas preserve database bytes and journal mode, without WAL/SHM files |
| L2 | Retain the exact listening socket while proving shutdown leaves new connections unaccepted by the server; await queue delivery with a bounded deadline | `983fe6817d701c7311fa51409b3c5bb7eab50f8d` | `shutdown_stops_accepting_on_original_listener`; three consecutive workspace passes |
| L3 | Ignore documented output-only insert fields while preserving server state; continue rejecting writable unsupported settings | `78a75e46a23a0b2502be7121aabccbfc0e12e365` | `bkt_ins_002_output_only_fields_do_not_override_server_state` and existing unsupported-field negatives |
| L4 | Readiness observes connection poison and permanently clears the ready flag | `78a75e46a23a0b2502be7121aabccbfc0e12e365` | `poisoned_catalog_permanently_revokes_readiness`: ready before panic, not-ready afterwards, subsequent catalog operations unavailable |
| L5 | Select a loopback port before startup and pass the same `--api-base-url`; document that the high-level SDK omits selfLink | `a6bf055d9cd12e43593e4e9570477a146704b196` | `bash scripts/sdk-go.sh`, vet and both Go integration tests pass without skips; script validates selected versus actual address |
| Info: signed int64 | Reject percent-encoded leading plus as well as an unescaped plus decoded to space | `78a75e46a23a0b2502be7121aabccbfc0e12e365` | `bkt_get_002_and_del_002_signed_conditions` for GET/DELETE and both condition parameters |
| Info: walkthrough cleanup | End the Compose walkthrough with `docker compose down` | `706ab65beb6e6af731faf41d949015d4e046f0b0` | Re-executed all Compose/CRUD/restart/reset/Go blocks through the final down; containers/network removed |

## Required verification

All Cargo/Rust commands used `env -u RUSTUP_TOOLCHAIN` with the rustup proxy first
in PATH. Rust is pinned to 1.99.1; Go is 1.27.3 with Storage v1.69.0 and API v0.306.0.

| Command | Result | Artifact |
| --- | --- | --- |
| `cargo fmt --check` | PASS, exit 0 | No output |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | PASS, exit 0 | [Clippy](bucket-clippy.txt) |
| `cargo test --workspace --locked` three consecutive times | PASS, 75 tests each time, no failures or ignored tests | [Rust](bucket-tests.txt) |
| `bash scripts/sdk-go.sh` | PASS, vet and both tests, no integration skip | [Go](bucket-sdk-go.txt) |
| `bash scripts/smoke-containers.sh linux/arm64 linux/amd64` | PASS on both platforms | [Containers](bucket-containers.txt) |
| `cargo xtask dependencies check` | PASS, exit 0; all inputs CURRENT, new-major probes included | [Dependencies](bucket-dependencies.txt) |
| `cargo xtask compatibility validate`, `render`, `check` | PASS; 588 rows, matrices unchanged | [Ledger](bucket-ledger.txt) |
| Getting-started Compose blocks through final down | PASS | [Guide](bucket-guide.txt) |
| Release build, native probes and SIGTERM shutdown | PASS | [Native](bucket-native.txt) |
| `bash -n scripts/sdk-go.sh`, `shellcheck scripts/sdk-go.sh`, `git diff --check` | PASS, exit 0 | No output |

## Decisions and limits

The ledger points containers to `8ccdda0f01ffae7965242b657bda21587a307e2a`, the
guide to `c9fcaa60af08501e99f25c1a1d4b524c9d606bf8`, the Go harness to
`3f5efc78903990d886422f7a9c045127eedb9cf9`, and protocol/catalog rows to their
actual adapter, integration or catalog commits. None points to the ledger-only fix.

Checked official source URLs and immutable proto revision/digests are in the
[slice contract](../slices/json-bucket-crud.md); Go major-path rules are in the
[dependency policy](../dependency-policy.md). Discovery has no readOnly flags on
these fields: the writable column and protobuf OUTPUT_ONLY markers support the
documented input choice. JSON zero and output-only input behavior were not checked
against real Cloud Storage. Runtime tests contacted only local endpoints.

The high-level Go type does not expose selfLink, so that SDK field is not checked;
Rust wire coverage remains authoritative for the configured origin. Choosing a
port before binding has a small race; a competing bind fails startup explicitly.
arm64 containers ran natively and amd64 under emulation. Native amd64, remote CI,
real-cloud conformance, objects, permissions and performance measurements remain
outside this task. No dependencies were added, no main commit or push was made,
and there are no open coordinator questions.

One intermediate shutdown-test attempt failed with WouldBlock before the bounded
accept wait was added; final consecutive runs pass. The resolved failure output
is retained in [the integration report](bucket-crud.md#resolved-failures-during-integration).
