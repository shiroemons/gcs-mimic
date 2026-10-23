# Actual-browser CORS verification

<!-- markdownlint-configure-file {"MD013": false} -->

The reusable fixture is [`cors-browser.html`](../../../scripts/cors-browser.html),
served from a distinct trusted HTTPS loopback origin by
[`cors-browser.sh`](../../../scripts/cors-browser.sh). It requires Node.js and
Chromium. It never disables certificate or hostname verification. Run it only in
an isolated browser environment whose NSS database trusts the emulator's public
development CA. The fixture refuses non-local HTTPS API endpoints and bucket
names outside its disposable `cors-browser-*` namespace.

## Executed result

On 2026-10-23, the recorded browser runtime passed both phases in both profiles:
strict initial (38 checks), permissive initial (36), strict restart (7) and
permissive restart (7). All four browser commands exited zero; the combined
[plain output](cors-browser.txt) records 88 passing checks. Chromium
156.0.8078.47 ran in an isolated Debian 12 container using the existing
`node:24.22.1-bookworm-slim` image. Only that container's NSS database trusted
the development CA. The browser origin was `https://127.0.0.1:54613`; the native
emulator was reached through `https://gcs:54475` (strict) and
`https://gcs:54476` (permissive), using a container host-gateway mapping.
The same CA also verified native-host readiness at the two
`https://127.0.0.1` API endpoints. Browser and host connections checked SANs;
no TLS verification bypass or host trust-store change was used.

The server stopped with SIGINT and exit zero between phases. Restart reused
each profile's own data directory and advertised endpoint. The restart phases
verified persisted rules/bytes and deleted all final fixture resources.

## Executed runtime and commands

The executed build includes custom-scheme/opaque-origin validation and HTTP/HTTPS
default-port equivalence in XML origin matching. The runtime was copied into
the isolated run directory so concurrent Cargo checks could not replace the
executable between phases. The copied runtime and source inputs had identical
SHA-256 values before the first server start and after the final browser phase.
Absolute temporary path prefixes in the digest below are replaced with
`<temporary-directory>`.

```sh
run_dir=$(mktemp -d)
cargo build --locked --offline
cp target/debug/gcs-mimic "$run_dir/runtime"
```

The two server commands ran in separate terminal processes and were repeated
with the same arguments after SIGINT and successful exit:

```sh
"$run_dir/runtime" serve --listen 0.0.0.0:54475 --api-base-url https://gcs:54475 \
  --data-dir "$run_dir/snapshot-strict" --auth-mode strict \
  --identity-file config/identities.json
"$run_dir/runtime" serve --listen 0.0.0.0:54476 --api-base-url https://gcs:54476 \
  --data-dir "$run_dir/snapshot-permissive"
```

Before each initial/restart browser phase, readiness was confirmed using normal
CA and IP SAN verification. The retry only waits for the known startup bind
delay; a final nonzero exit fails the gate:

```sh
curl --disable --noproxy '*' --silent --show-error --fail --max-time 3 \
  --retry 65 --retry-delay 1 --retry-connrefused \
  --cacert src/tls/development-ca.pem https://127.0.0.1:54475/_internal/ready
curl --disable --noproxy '*' --silent --show-error --fail --max-time 3 \
  --retry 65 --retry-delay 1 --retry-connrefused \
  --cacert src/tls/development-ca.pem https://127.0.0.1:54476/_internal/ready
docker exec gcs-mimic-cors-browser bash scripts/cors-browser.sh \
  https://gcs:54475 cors-browser-strict strict initial
docker exec gcs-mimic-cors-browser bash scripts/cors-browser.sh \
  https://gcs:54476 cors-browser-permissive permissive initial
```

After stopping/restarting both servers and passing readiness again:

```sh
docker exec gcs-mimic-cors-browser bash scripts/cors-browser.sh \
  https://gcs:54475 cors-browser-strict strict restart
docker exec gcs-mimic-cors-browser bash scripts/cors-browser.sh \
  https://gcs:54476 cors-browser-permissive permissive restart
```

## Browser input digests

Executed SHA-256 input snapshot (rechecked unchanged after all four browser phases):

```text
769447146c243564bf9a2e56c4885eb6aa014ffe3262a816ac883f3835814c13  <temporary-directory>/runtime
56fe1e0ce37adbda9150377db8e92126a136a789305ed9ce2957f1fae025226e  src/server.rs
d9ea971114ee682a344c3de3f2f78819a13b8ad090c33426098d135acaca1960  src/json/cors.rs
bf322d9d2e83321e4834d9bab8f1f522dd29881a6c27187407e78b42d3a191b8  src/xml/cors.rs
02b17864f9eb5f2f42b28e9ed3a07ef81cf46dd9b0e07db83af771a8fb019e9e  src/bucket.rs
2f72fe1b71784ca1ec001bbd1a0d137f092610b6e3720aedfb4e14c89c839e6e  src/tls.rs
8ad5d7e7df1b46f28da1b7dbd6a6769f68eb99111972f68725ccda14e340c4c6  src/tls/development-ca.pem
9a541b4d8884d5f27e0f303654bc00efd2bedbeb0f8cc27a4119f5d96c244d76  src/tls/development-cert.pem
f1f2ff060bc6f3a5531407044c14ce7212b917442622c0eae8b1db37193cbd82  src/tls/development-key.pem
821e03bb0ba30a9b84429c4b3a96e6b21f5379df8891612385f994b4d3839ac2  Cargo.lock
d13306be0ecda81ce779baa290ffb417380d525928477f16200884b5169cca33  rust-toolchain.toml
```

These are prior-source browser results. The subsequent resource fix changes
only catalog bucket-list page admission and server continuation handling;
JSON/XML CORS parsing, matching, wrapping and TLS inputs above remain unchanged.
The server source digest consequently differs in the final build. Final-source
Rust and ARM64 resource regressions are recorded in the [combined evidence](cors.md);
the browser runtime hash above does not identify that later image or a second
browser execution.

## Reproduce

Start an isolated emulator bound to an address reachable from the browser, with
an advertised HTTPS API endpoint using a matching certificate SAN. For the strict
profile, use `config/identities.json` and project `local-project`. Import
`src/tls/development-ca.pem` into the throwaway Linux browser's NSS database:

```sh
mkdir -p "$HOME/.pki/nssdb"
certutil -N -d "sql:$HOME/.pki/nssdb" --empty-password
certutil -A -d "sql:$HOME/.pki/nssdb" -n gcs-mimic-cors -t C,, \
  -i src/tls/development-ca.pem
bash scripts/cors-browser.sh https://gcs:54447 cors-browser-strict strict initial
```

Restart the same emulator with the same data directory, profile and advertised
endpoint, then execute:

```sh
bash scripts/cors-browser.sh https://gcs:54447 cors-browser-strict strict restart
```

Wait for the emulator's `ready` event before each phase. Rebinding the same
listener port can take time after shutdown.

The restart phase verifies persisted CORS and XML bytes, then removes the fixture
objects and bucket. Failed runs retain resources for inspection. Repeat with an
isolated permissive server, a different bucket and `permissive` in place of
`strict`. `CORS_BROWSER_PORT` selects the origin port (default `54613`); retain
that port between phases because the persisted rule contains the exact origin.
The `gcs` hostname must resolve to the emulator, for example through an isolated
container network or a container's `--add-host=gcs:host-gateway` mapping. Native
loopback API origins are supported too. No production credentials are used.

## Cases

- JSON bucket create/get/list/PATCH/PUT and deletion, object media/multipart and
  resumable upload, exact-byte download, listing, metadata PATCH/PUT, field
  selection and conditional 304.
- Browser-readable ETag and HTTPS resumable Location, including actual PUT to
  that Location.
- A simple JSON request using `credentials: include` remains browser-readable;
  the strict server still returns 401 when no identity is provided. Bearer-token
  requests use the browser's default credentials mode. The scoped JSON contract
  does not promise credentialed preflight with `credentials: include`.
- Strict JSON 403 after successful preflight, with a readable JSON error and no
  update grant.
- XML GET/HEAD, requested-header preflight, configured exposed ETag/generation,
  denied header/method/origin, wildcard origin, configuration clearing and
  credentialed-fetch rejection. Strict XML 403 remains readable after an allowed
  preflight and carries `AccessDenied`.
- JSON still works when bucket rules reject the browser origin. Persisted XML
  rules and object bytes survive an emulator restart.

Fetches use `cache: no-store` to observe configuration changes at the server.
This does not claim that revocation invalidates browser or intermediary caches.
Real V4 RSA signed XML PUT/GET/HEAD, signer permission denials and restart are
covered separately by the [signed browser journey](signed-browser.md), using
the same trusted HTTPS method.

The contract comes from Google's
[CORS reference](https://docs.cloud.google.com/storage/docs/cross-origin), checked
2026-10-23. JSON defaults and XML bucket rules are exercised separately. This is
local-browser evidence, not a real-Google differential test.
