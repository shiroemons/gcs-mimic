# Actual-browser signed transfers

<!-- markdownlint-configure-file {"MD013": false} -->

Executed **2026-10-23** against the native strict emulator. The reusable
[`signed-browser.sh`](../../../scripts/signed-browser.sh) prepares URLs using the
project's own offline `url sign` CLI, then serves
[`signed-browser.html`](../../../scripts/signed-browser.html) from a distinct
trusted HTTPS origin. The browser uses each signed URL unchanged: the final
`https://gcs:54477` host, port, path and query are signed before transfer.

Both browser commands exited **0**: strict initial **51 PASS** lines and strict
restart **10 PASS** lines, **61** total. The [plain output](signed-browser.txt)
contains the actual browser output and the final empty-catalog JSON response.
No server change was required by these browser cases.

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
  the separately integrated wire-test work; they were not run in this checkout.
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
resource measurements or full guide/SDK/container-suite execution.

The local contract was checked against Google's [CORS reference](https://docs.cloud.google.com/storage/docs/cross-origin)
and [signed URL overview](https://docs.cloud.google.com/storage/docs/access-control/signed-urls)
on 2026-10-23. Existing provider edge gaps stay open; these results are local
Chromium evidence.

## Source and validation

Native runtime source baseline: `760b12a320719a385301492decdafe7f6234012a`.
Harness commit: `8fe61a7712340f5fc9289725a51ba6a48b9f74d9`. Runtime and protocol
inputs were unchanged across both phases; the runtime is copied before launch
to keep concurrent Cargo commands from replacing it.

```text
642159030e3b677b10d6e83d889d48355f388ffbecf738c4dac9978b56ca6061  target/signed-browser-run/runtime
f9a8f77a272ccc40017fb3f1de15d73ae97b68a6cb1936bdb1a4f537cb67a427  src/server.rs
bf322d9d2e83321e4834d9bab8f1f522dd29881a6c27187407e78b42d3a191b8  src/xml/cors.rs
c6a4c2ccf06ddee783f7cd327109f6188c0a54c3a17e9d9a7b5eddffae011e46  src/xml/put.rs
9e1db1ca25999c69863ed86215ac46167ceca17291664b45b0839af6e3bed47d  src/auth/signature.rs
8ad5d7e7df1b46f28da1b7dbd6a6769f68eb99111972f68725ccda14e340c4c6  src/tls/development-ca.pem
64a0f455de853d96d9f5fa36e6efb1815d200c9b94f13b83526acbec4a966ea8  config/identities.json
57b6107385fe009574ac09a52fe8a818d839cae5ee37abc95fe0955a70c3c024  scripts/signed-browser.sh
1f2d1e1a16dcceb7d3eaa25f85d43dcc16716bb21bee992f76cd7ec97441d1ca  scripts/signed-browser.html
3b8100d00d9d1b534fce001b07e8329cd2d26aaba19bdfe4678dd79ae718116b  Cargo.lock
```

All final commands exited zero:

```sh
bash -n scripts/signed-browser.sh
cargo fmt --check
cargo clippy --all-targets --locked --offline -- -D warnings
cargo test --locked --offline --test auth_http --test sign_url_cli --test tls
```

Rust results: `auth_http` **93 passed**, `sign_url_cli` **4 passed**, `tls`
**9 passed**, no ignored or failed tests. These existing targets cover CORS,
signing, helper behavior and verified TLS. The new browser fixture itself
contains the end-to-end assertions; no new Rust logic was introduced.
