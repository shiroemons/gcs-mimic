# Actual-browser signed transfers

<!-- markdownlint-configure-file {"MD013": false} -->

Final integrated verification executed **2026-10-23 UTC** against source
`2218d001b3fe4e158f05d9d6c62a0eb68c986d84`. The reusable
[`signed-browser.sh`](../../../scripts/signed-browser.sh) prepares URLs using the
project's own offline `url sign` CLI, then serves
[`signed-browser.html`](../../../scripts/signed-browser.html) from a distinct
trusted HTTPS origin. The browser uses each signed URL unchanged: the final
`https://gcs:54477` host, port, path and query are signed before transfer.

Both browser commands exited **0**: strict initial **51 PASS** lines and strict
restart **10 PASS** lines, **61** total. The [plain output](signed-browser.txt)
contains the actual browser output and the final empty-catalog JSON response.
The integrated server retains CORS on signed errors and malformed object names,
selects the bucket independently of query validity, and rejects invalid
virtual-host bucket names before preflight.

## Runtime and trust

| Input | Executed value |
| --- | --- |
| Container base | `node:24.22.1-bookworm-slim`, Linux ARM64 |
| Image ID and repository digest | `sha256:1681862882da07ee5476fee047bcf34138833f1df42633bb25bcb0f438d5bf3d` |
| Node, native preparation and container | `v24.22.1` |
| Chromium | `156.0.8078.47`, Debian package `156.0.8078.47-1~deb12u1` |
| NSS tools | `2:3.87.1-1+deb12u4` |
| Native compiler | `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, `aarch64-apple-darwin`, LLVM `23.1.1` |
| Cargo | `1.99.1 (5f94df478 2026-08-27)` |
| Browser page origin | `https://127.0.0.1:54614` |
| API origin signed and requested | `https://gcs:54477` |
| Identity registry | `config/identities.json` |
| Signer and test-only key | `local-viewer@example.invalid`, `fixtures/signing/test-only-private.pem` |

The throwaway container maps `gcs` to `host-gateway`. Only its NSS database adds
the development CA (`gcs-mimic-development`, trust `C,,`); the host trust store
is untouched. Both browser origins use normal CA and SAN verification. No TLS,
hostname or web-security bypass is enabled. Chromium's `--no-sandbox` is used
only because the disposable container runs as root. The fixture otherwise uses
the same headless runtime flags as the existing CORS browser harness.

The official Rust stable manifest was fetched at slice start and reports release
`1.99.1`, dated `2026-10-15`. No runtime, test or SDK dependency was added or
changed; the existing locked application graph was used.

## Reproduce the executed method

Run preparation on the native host; the Linux browser does not execute the
native macOS binary. The preparation phase writes a bounded fixture manifest,
which the browser container reads through the checkout's read-only mount.
Use only isolated local endpoints and disposable `signed-browser-*` buckets.
The script refuses other endpoint hosts and bucket namespaces. These are
deliberately public local identities and signing keys, never Google credentials.

From the checkout on the native host:

```sh
cargo build --locked --offline
mkdir -p target/signed-browser-run
cp target/debug/gcs-mimic target/signed-browser-run/runtime
```

Keep this server running in a separate terminal:

```sh
target/signed-browser-run/runtime serve --listen 0.0.0.0:54477 \
  --api-base-url https://gcs:54477 --data-dir target/signed-browser-run/data \
  --auth-mode strict --identity-file config/identities.json
```

Create the disposable browser environment and install the browser in that
container, without building or changing an application image:

```sh
docker run --detach --name gcs-mimic-signed-browser --add-host=gcs:host-gateway \
  --volume "$PWD:/workspace:ro" --workdir /workspace \
  node:24.22.1-bookworm-slim sleep infinity
docker exec gcs-mimic-signed-browser sh -c '
  apt-get update > /var/log/browser-install.log 2>&1 &&
  apt-get install -y --no-install-recommends chromium libnss3-tools >> /var/log/browser-install.log 2>&1 &&
  mkdir -p "$HOME/.pki/nssdb" &&
  certutil -N -d "sql:$HOME/.pki/nssdb" --empty-password &&
  certutil -A -d "sql:$HOME/.pki/nssdb" -n gcs-mimic-development -t C,, \
    -i src/tls/development-ca.pem
'
```

Wait for the server's `ready` event, verify readiness with CA/IP SAN checking,
then prepare and run the initial phase:

```sh
curl --disable --noproxy '*' --silent --show-error --fail --max-time 3 \
  --cacert src/tls/development-ca.pem https://127.0.0.1:54477/_internal/ready
GCS_MIMIC_BIN=target/signed-browser-run/runtime bash scripts/signed-browser.sh \
  prepare https://gcs:54477 signed-browser-strict initial > target/signed-browser-run/initial.json
docker exec --workdir /workspace/scripts gcs-mimic-signed-browser \
  bash signed-browser.sh run ../target/signed-browser-run/initial.json
```

Stop the native server with **SIGINT** and wait for its zero exit and `stopped`
event. Repeat the exact server command above, keeping the same data directory,
registry and endpoint. After the new `ready` event:

```sh
curl --disable --noproxy '*' --silent --show-error --fail --max-time 3 \
  --cacert src/tls/development-ca.pem https://127.0.0.1:54477/_internal/ready
GCS_MIMIC_BIN=target/signed-browser-run/runtime bash scripts/signed-browser.sh \
  prepare https://gcs:54477 signed-browser-strict restart > target/signed-browser-run/restart.json
docker exec gcs-mimic-signed-browser bash scripts/signed-browser.sh \
  run target/signed-browser-run/restart.json
curl --disable --noproxy '*' --silent --show-error --fail \
  --cacert src/tls/development-ca.pem --header 'Authorization: Bearer local-admin-token' \
  'https://127.0.0.1:54477/storage/v1/b?project=local-project'
docker rm --force gcs-mimic-signed-browser
```

Each prepared URL expires 15 minutes after preparation, except the deliberately
expired negative fixture. Run the initial phase promptly and always rerun
`prepare` with `restart` after restarting the server; retained manifests do not
extend their expiry window.

The restart phase signs fresh URLs, verifies persisted CORS and exact uploaded
bytes, and deletes the uploaded object and bucket. A failed phase retains
emulator resources for diagnosis. `SIGNED_BROWSER_PORT` selects the page origin
port (default `54614`); retain it between phases. `CHROMIUM_BIN` selects an
installed browser executable. Manifests contain local signed capabilities;
do not publish them. Successful fetches use the original manifest strings,
`redirect: error` and `cache: no-store`.

## Verified cases and boundaries

- JSON bucket configuration permits XML `PUT`, `GET` and `HEAD` from the exact
  page origin, with zero preflight cache age and explicit request/exposure
  headers. Signed PUT uses `application/octet-stream` and signed
  `x-goog-meta-browser`, forcing a real browser preflight.
- PUT succeeds, GET returns exact bytes and independently signed HEAD returns
  the exact length. JSON metadata confirms the signed Content-Type, custom
  metadata and metageneration are stored, and signed GET exposes that metadata.
  JavaScript reads configured ETag,
  generation and hash on all three, while non-exposed metageneration is hidden.
  Signed Content-Type and Content-Disposition response overrides also work.
- Changing the rule to a disallowed origin blocks signed GET and PUT. Denied
  request header and method preflights block publication. Independent
  authenticated JSON reads compare complete object metadata and bytes and
  confirm that the new target remains absent. Each blocked fetch is paired with
  the same signed URL succeeding after restoring the allowed rule, request
  headers or method; this distinguishes CORS denial from transport failure.
- Revoking the signer's bucket object-admin grant leaves its registered
  project object-viewer role. Allowed preflight still leads to readable
  **403 AccessDenied** for overwrite and new-object PUT, preserving state.
  This registry has no role-less public-key identity: a valid signed GET denied
  specifically for missing read permission is not exercised by this browser
  fixture. Anonymous private GET and invalid signatures are separate checks.
  The integration wire suite in `tests/signed_browser_http.rs` covers role-less
  signed GET/HEAD/PUT denial under matching CORS with PAP through
  `signed_preflight_never_grants_permission_and_pap_preserves_signer_access`
  and `signed_failures_keep_cors_and_preserve_object`. Those tests belong to
  the integrated wire suite; all nine tests passed in this checkout's final
  `just check` run.
- Tampered signature, expired URL and changed signed metadata header yield
  readable **403 SignatureDoesNotMatch**, **400 ExpiredToken** and **403
  SignatureDoesNotMatch**, respectively, without publication. Anonymous private
  XML GET and PUT yield readable **401 AuthenticationRequired** despite CORS.
- SIGINT/restart on the same data directory preserves the CORS rule and bytes;
  a fresh signed GET works before fixture deletion.

The strict profile supplies the signature and permission evidence. No permissive
phase was run: that profile deliberately bypasses cryptographic verification and
authorization, so it would not add evidence for those claims. Virtual-host
browser access is not exercised because the built-in certificate does not cover
bucket-prefixed hostnames; supplied SAN-matching certificates remain a separate
case. This does not claim real Google, iOS, browser-cache revocation, large-object
resource measurements, the entire guide or real Rails integration. The affected
native guide blocks, SDK suites and both container architectures were executed
as recorded below.

The local contract was checked against Google's [CORS reference](https://docs.cloud.google.com/storage/docs/cross-origin)
and [signed URL overview](https://docs.cloud.google.com/storage/docs/access-control/signed-urls)
on 2026-10-23. Existing provider edge gaps stay open; these results are local
Chromium evidence.

## Source and validation

Native runtime source baseline: `2218d001b3fe4e158f05d9d6c62a0eb68c986d84`.
Harness commit: `8fe61a7712340f5fc9289725a51ba6a48b9f74d9`. Runtime and protocol
inputs were unchanged across both phases; the runtime is copied before launch
to keep concurrent Cargo commands from replacing it.

```text
186c0a9d236ac669fc79af4bc54ef54d21817434cda5a0f2750dae43942a3ab9  target/signed-browser-run/runtime
a3bb1adfbb1400b9e7f52bbad3ce7c32630e1f6ec859cbd9c8ba2d2368fd5e5c  src/server.rs
4bef6cf37c8325425ed6b855b4e45f4bd5e798eedf3118af773256d77b08b601  src/xml/mod.rs
bf322d9d2e83321e4834d9bab8f1f522dd29881a6c27187407e78b42d3a191b8  src/xml/cors.rs
c6a4c2ccf06ddee783f7cd327109f6188c0a54c3a17e9d9a7b5eddffae011e46  src/xml/put.rs
9e1db1ca25999c69863ed86215ac46167ceca17291664b45b0839af6e3bed47d  src/auth/signature.rs
8ad5d7e7df1b46f28da1b7dbd6a6769f68eb99111972f68725ccda14e340c4c6  src/tls/development-ca.pem
64a0f455de853d96d9f5fa36e6efb1815d200c9b94f13b83526acbec4a966ea8  config/identities.json
57b6107385fe009574ac09a52fe8a818d839cae5ee37abc95fe0955a70c3c024  scripts/signed-browser.sh
1f2d1e1a16dcceb7d3eaa25f85d43dcc16716bb21bee992f76cd7ec97441d1ca  scripts/signed-browser.html
3b8100d00d9d1b534fce001b07e8329cd2d26aaba19bdfe4678dd79ae718116b  Cargo.lock
```

## Combined final verification

All executions below used the same final runtime source on **2026-10-23 UTC**
(the native guide and final SDK completion crossed midnight into 2026-10-24
in Asia/Tokyo). Documentation/ledger updates do not change that runtime.
Environment: macOS 27.0.1, ARM64, Rust/Cargo 1.99.1, Node 24.22.1 and Docker
client/server 29.9.2. Linux ARM64 is native; AMD64 uses Docker Desktop emulation.
The [combined transcript](signed-browser.txt) contains real command output,
including the failed full-test output and the successful focused recheck.
Local path prefixes are redacted; no signed manifests or private generated keys
are included.

| Executed command or journey | Exit | Final observation |
| --- | ---: | --- |
| `just check` | 0 | Pinned fetch/fmt/Clippy, shell checks, compatibility check and **665 workspace tests** pass, zero failed/ignored; signed wire **9**, auth/CORS **93**, TLS **9** |
| `cargo test --locked` | 101 | Existing `resumable_http::object_caps_apply_to_known_unknown_and_streamed_lengths` fails with `Connection reset by peer`; that target has 32 passed, 1 failed |
| `cargo test --locked --test resumable_http object_caps_apply_to_known_unknown_and_streamed_lengths -- --exact` | 0 | The failed test passes alone; the same test also passed in `just check`, without source changes |
| `signed-browser.sh prepare` and `run`, strict initial/restart, as above | 0 each | **51 + 10 = 61** checks, normal SIGINT/zero exit between phases, fresh signatures and empty final catalog |
| `cors-browser.sh https://gcs:54475 cors-browser-strict strict initial` and `restart` | 0 each | **38 + 7** checks |
| `cors-browser.sh https://gcs:54476 cors-browser-permissive permissive initial` and `restart` | 0 each | **36 + 7** checks; existing CORS total remains **88**, so historical `cors-browser.txt/.md` results are unchanged |
| `GCS_MIMIC_SDK_SCHEME=https just check-sdk` | 0 after environment recovery | Go 1.27.3, Storage 1.70.0/API 0.308.0: vet and **46** top-level tests; Ruby 4.0.9/Bundler 4.0.23/Storage 1.62.1: origin guard and both profiles, including official signed GET/PUT |
| `just check-containers` | 0 | Both architectures build and pass isolated probes/CRUD, CORS, read-only/resource settings, persistence, seed/reset and signals |
| `cargo xtask dependencies check` | 0 | All 36 compiler/library/SDK/container inputs report CURRENT; no manifest or lockfile change |
| Native signed guide: shell blocks under “Signed private downloads and uploads” | 0 | Private GET/PUT, method/object/host/expiry/header denials, 412 preservation, overrides, PAP, virtual hosts and public reactivation |
| Native CORS guide: shell block under “Browser CORS and mobile application origins” | 0 | Exact/default-port/custom origins, preflight, independent JSON, denied origin, stale 412 and fixture deletion |
| Signed-browser guide subsection | 0 | Preparation/run blocks use the native fixed binary and isolated trusted Chromium; the initial evidence command also checks caller-relative manifest resolution |
| `cargo xtask compatibility render` and `cargo xtask compatibility check` | 0 each | 591-row ledger and both generated English/Japanese views agree |
| `npx --yes markdownlint-cli2` on the five hand-edited Markdown files and both generated matrices | 0 each | No issues; matrices use the existing long-table convention and MD024 `siblings_only` |
| `git diff --check` on changed source/docs and repository-wide local-path `git grep -nE` | 0 / 1 | No whitespace issues outside preserved raw output; no local-path matches |

The standalone full `cargo test --locked` gate is **not green**. Its exact
failure is retained rather than replaced with the passing recheck. No negative
assertion, timeout, error handling or test was weakened. This existing
resumable transport failure is outside the signed-browser acceptance; its cause
is not established by a single passing recheck. All signed-browser and CORS
acceptance tests passed in the complete final `just check` run. The behavior rows
BEHAVIOR-BUCKET-009 and DESIGN-051/052/053 retain IMPLEMENTED for their passing
scope; UX-007 remains PARTIAL with the failed expanded final regression case.
No later source
edits or additional full/container/browser runs were made to hide the failure.

The first two SDK attempts exited 1 after all Go tests passed: an inherited
version-manager shim selected Ruby 3.4.1 for `bundle exec ruby`, contrary to
the 4.0.9 Gemfile pin. The successful recovery uses a PATH without those shims
and the Ruby 4.0.9 prefix matching the cached gem native extensions. Both the
matching Ruby prefix and gem cache were selected externally; repository sources,
Gemfile and lockfile were unchanged. HTTPS uses the built-in development CA;
Go's virtual-host case asserts its expected SAN rejection, while existing
HTTP wire tests exercise successful virtual-host transfers. No HTTPS virtual-host
success is inferred.

Final application image/index IDs:

| Architecture | Image/index ID |
| --- | --- |
| Linux AMD64, emulated | `sha256:8b5e609cf491eccc6f1516da27942416df27e21d297012f0e76747cef9a48eb3` |
| Linux ARM64, native | `sha256:a2b5985297e14247d1df453c9f1a5c76157151cd560e0e0472b1b20448f4dcd9` |

The unchanged nine-test signed wire suite covers path-style and virtual hosts,
full-query unauthenticated OPTIONS, origin/method/header denials, 400/401/403/412
CORS and failed-write preservation, signed headers, role-less signers, PAP,
clearing/replacement, persistence, JSON independence, two-bucket selection,
decoded slash/space names and malformed `%ZZ`/dot names. The added invalid-host
case retains 400 with no CORS. Provider OPTIONS examples, exact error/Vary
headers and malformed-input precedence remain OPEN in SPEC-CORS-EDGES and
SPEC-SIGNING-EDGES; local passing tests do not resolve them.

`SPEC-SIGNED-BROWSER-CORS` consolidates the additional OPTIONS/example and
signed-error questions without closing either earlier gap. Markdown validation
keeps the hand-edited documents' existing MD013 convention. Generated matrices
also disable MD013 for their wide tables and permit repeated category headings
only under distinct parent sections through MD024 `siblings_only`; all other
rules stay enabled. Raw Docker/guide output is preserved, including trailing
spaces, so whitespace checks exclude only the combined `.txt` transcript.

The runtime and all listed source digests were rechecked unchanged after the
browser phases. All four native verification listeners stopped with SIGINT and
exit zero, and the disposable browser container was removed. Only local test
resources were used; no host trust setting, Google resource or publication was
changed. Generated signed manifests were removed after execution.
