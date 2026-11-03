# HTTP adapter boundaries

Management routes, including sync controls, refuse every Origin header and every
Sec-Fetch-Site value other than `none` with 403 before route-specific gates.
Tools that send neither header keep working, including multipart fixtures and
the empty-body delete_all alias. A public DNS Host receives 421 unless it is a
configured API/public host, virtual-host base domain or `admin_allowed_hosts`
entry. IP literals, single-label service names and reserved local DNS suffixes
are allowed independently of ports. Host matching is case-insensitive and strips
one trailing dot only for this guard; XML routing keeps its separate contract.
Duplicate Host headers and invalid authorities receive 400. This protects
against browser CSRF and DNS rebinding without changing Google API permissions.

`admin_allowed_hosts` accepts up to 32 comma-separated DNS names. Configure it
through `--admin-allowed-hosts`, `GCS_MIMIC_ADMIN_ALLOWED_HOSTS` or flat JSON.

Unknown shared-listener routes return a JSON `404 notFound` envelope with
`error.code`, `error.message` and `error.errors` entries. Invalid XML bucket
paths retain this JSON boundary. Health and readiness probes retain their local
status documents. A zero Content-Length, including `00`, denotes no body;
Transfer-Encoding and malformed nonzero lengths are treated conservatively.
JSON media 304 construction tolerates absent optional representation headers.

Invalid object page tokens return a fixed error message without echoing the
client's token. CORS lookup diagnostics retain the existing backend allow-list;
I/O causes expose only ErrorKind, never the underlying filename or message.

Body collection uses amortized buffer growth within the existing caps and
preallocates an exact bounded size hint. Its generic Buf loop remains necessary
for noncontiguous body data. Glob matching reuses one traversal stack for the
whole name. Hexadecimal decoding uses the shared codec. Origin syntax checks,
resumable query sorting, legacy bucket response conversion and connection timers
avoid repeated allocations without changing their declared protocol behavior.
No comparative throughput or memory improvement was measured.

## Verification

Native checks on 2026-11-03 used `CARGO_BUILD_JOBS=4`:

- `cargo check --all-targets`: passed.
- `cargo test --lib json:: -- --test-threads=4`: 119 passed.
- `cargo test --lib xml::error_tests -- --test-threads=4`: 3 passed.
- `cargo test --lib match_glob:: -- --test-threads=4`: 2 passed.
- `cargo test --lib server::routing::tests -- --test-threads=4`: 1 passed.
- `cargo test --lib server::deadline_claim_tests -- --test-threads=4`: 4 passed.
- `cargo test --lib unknown_routes_use_json_error_envelopes_and_probes_accept_zero_lengths -- --test-threads=4`: 1 passed.
- `cargo xtask lint`: 0 production violations.
- `cargo fmt --all --check`: passed.

The upload-frame module ran four passing tests and one environment failure:
`server::upload_frame_tests::upload_frames_stay_below_twice_the_connection_read_buffer`
could not bind a local socket:

```text
Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }
test result: FAILED. 4 passed; 1 failed; 0 ignored
```

This sandbox result does not verify socket behavior. Existing wire tests keep
their negative assertions and require execution outside the restricted sandbox.
No SDK suite, container build or real Google Cloud request was run.

Final-source host checks on 2026-11-04 passed workspace/all-targets Clippy,
production lint (zero violations), formatting, compatibility generation/check
and the documentation gate. The three shell self-tests and offline container
publication/mirroring tests also passed; no image was built.

`GCSM_CHECK_CROSS=0 just check` stops in the existing `test-check.sh` self-test,
which inherits the switch but expects mocked cross-target commands. With that
variable removed the self-test passes. The host gate was therefore run directly:

```sh
GCSM_CHECK_CROSS=0 CARGO_BUILD_JOBS=4 env -u RUSTUP_TOOLCHAIN \
  bash scripts/with-test-tmpdir.sh bash scripts/check.sh
```

The library run completed with 1100 passing and 121 failing tests. All 121 failure
blocks contain PermissionDenied or Operation not permitted, including the accept
recovery/drain and upload-frame socket tests. Process-based wire tests also cannot
start listeners in this sandbox; some report EOF while reading the missing ready
event rather than the underlying listener diagnostic. This is a failed full test
gate, not passing wire evidence. No negative assertion was removed or skipped.
The documentation and offline shell checks following the Rust gate were run
separately because the gate stops after a failing test binary.
