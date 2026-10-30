# Local HTTPS with an explicit development CA

<!-- markdownlint-configure-file {"MD013": false} -->

Part of the [setup and integration guides](getting-started.md#guides). Run the
shell blocks from the repository root in one Bash session.

HTTPS starts without certificate preparation. The binary embeds a development
certificate for `localhost`, `gcs`, `127.0.0.1` and `::1`; `tls ca` prints its
public CA without reading configuration or opening the data directory/network.
The shared development server key is deliberately public. Keep this emulator
isolated; custom certificate/key pairs remain available below. This standalone
journey uses Bash, curl and jq and changes no system trust store.

```sh
tls_work=$(mktemp -d)
cert_dir="$tls_work/certs"
mkdir "$cert_dir"
cargo build --locked
./target/debug/gcs-mimic tls ca > "$cert_dir/ca.pem"
./target/debug/gcs-mimic serve --listen 127.0.0.1:54443 \
  --api-base-url https://localhost:54443 --data-dir "$tls_work/data" \
  > "$tls_work/native.log" 2>&1 &
tls_pid=$!
for attempt in $(seq 1 50); do
  if ./target/debug/gcs-mimic healthcheck --url https://localhost:54443/_internal/ready; then break; fi
  sleep 0.1
done
bash scripts/tls-smoke.sh https://localhost:54443 "$cert_dir/ca.pem"
kill -TERM "$tls_pid"
wait "$tls_pid"
```

`--scheme https` is the default; omitting both TLS paths uses the built-in pair.
To use HTTP, explicitly select `--scheme http` and omit the TLS paths. Custom
TLS needs both `--tls-cert` and `--tls-key`; a single path fails. The supplied
PEM chain (leaf first) and private key are loaded and checked before recovery,
seeding, binding or readiness. Each is capped at 1 MiB. TLS 1.2/1.3 and HTTP/1.1
are supported; handshakes share `max_connections` and have a separate
`--tls-handshake-timeout-ms` deadline (default 5000, range 1–60000).
Configuration precedence is CLI > `GCS_MIMIC_` environment > JSON > defaults.
For example, use JSON `scheme`, `tls_cert`, `tls_key`, `tls_handshake_timeout_ms`
or environment `GCS_MIMIC_SCHEME`, `GCS_MIMIC_TLS_CERT`, `GCS_MIMIC_TLS_KEY`.
An omitted API base uses the listener scheme and actual bound address, including
the allocated port for `--listen 127.0.0.1:0`. Wildcard `0.0.0.0` / `::` becomes
`127.0.0.1` / `[::1]`. Ready reports the effective `api_base_url`.
Set `--api-base-url` for a different client-reachable address or proxy prefix.
Explicit HTTP(S) API bases remain independent for
reverse proxies. Neither `Host` nor forwarding headers change generated links.

Build and run the same journey with the non-root, read-only source image:

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-tls-demo-$$
export GCS_MIMIC_IMAGE=gcs-mimic:tls-dev
export GCS_MIMIC_PORT=54444
export GCS_MIMIC_API_BASE_URL=https://localhost:54444
docker build -t "$GCS_MIMIC_IMAGE" .
docker compose config --quiet
docker compose up -d --no-build --wait
bash scripts/tls-smoke.sh "$GCS_MIMIC_API_BASE_URL" "$cert_dir/ca.pem"
docker compose stop
docker compose start --wait
docker compose exec -T --interactive=false gcs \
  gcs-mimic healthcheck --url https://localhost:4443/_internal/ready
```

For peers that follow generated links, set `GCS_MIMIC_API_BASE_URL=https://gcs:4443`
and recreate the service before starting their upload. Host clients and browsers
use `https://localhost:54444`. Both names are in the certificate; a container's
`localhost` names that container. Mount only `ca.pem` in client containers and
pass it to their TLS client. The public URL helper accepts an independent base
and grants no access. Same-origin browser navigation works after explicitly
trusting the development CA in an isolated browser profile; cross-origin fetch
uses JSON defaults or the configured XML bucket rules. Never disable certificate
or hostname verification. The [TLS evidence](evidence/local-tls.md) records the
isolated Chromium trust procedure and failure checks.

The Docker image and `compose.yaml` default to built-in HTTPS without TLS mounts.
The built-in leaf expires on 2036-10-26 and its CA on 2036-10-27; renew the
embedded assets before then with `scripts/tls-development-cert.sh` as described
in the [local TLS contract](slices/local-tls.md#bundled-development-certificate).
The chain carries the key identifiers that strict verifiers such as Python
3.13+ require, so clients keep their default certificate checks. A CA exported
from gcs-mimic 0.5.0 or earlier does not verify the current certificate; export
it again with `gcs-mimic tls ca`. To use a custom development CA instead, generate
throwaway two-day fixtures with OpenSSL and select the read-only TLS override:

```sh
custom_cert_dir="$tls_work/custom-certs"
bash scripts/tls-fixtures.sh "$custom_cert_dir"
export GCS_MIMIC_TLS_DIR="$custom_cert_dir"
docker compose -f compose.yaml -f compose.tls.yaml up -d --no-build --wait
bash scripts/tls-smoke.sh "$GCS_MIMIC_API_BASE_URL" "$custom_cert_dir/ca.pem"
```

Custom probes use `--ca-cert /tls/ca.pem`; the default probe trusts only the
built-in CA. Explicit `--ca-cert` replaces that trust rather than adding it.
`compose.http.yaml` explicitly selects HTTP for the detailed
[HTTP journey](getting-started.md#build-and-start-with-compose).
Add `compose.strict.yaml` to require the existing local test identities.
Stopping/recreating preserves the named data volume. `docker compose ... down`
stops/removes this demo's containers without resetting its data. To reset only
this disposable project's local catalog, first stop it and explicitly remove
its named volume; never reset another project's data.

For official clients, run the [Go HTTPS suite](../../sdk-tests/go/README.md#https-with-an-explicit-development-ca)
and [Ruby HTTPS journey](../../sdk-tests/ruby/README.md) using the same CA and
advertised origin. Their endpoint guards prevent fallback to Google or metadata
servers. The Rust [wire tests](../../tests/tls.rs) cover untrusted CA, wrong
hostname, expiry, invalid files, stalled admission, strict grants and restart.
After all clients and containers stop, delete only `$tls_work`; it contains
this journey's disposable certificates and native data.
