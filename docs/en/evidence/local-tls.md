# Local HTTPS/TLS verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-10-23 for the [frozen TLS contract](../slices/local-tls.md).
DESIGN-065 covers the declared local transport scope. DESIGN-055 gains HTTPS
link evidence but retains its public endpoint/proxy/signing remainder.

## Runtime and trust checks

The HTTPS default embeds a public development certificate/key and needs no
certificate preparation. Optional custom PEM pairs replace it; one-sided or
invalid overrides fail. Explicit HTTP needs no certificate. CLI/environment/JSON precedence, default
advertised scheme and invalid settings pass `tls_configuration_precedence_and_validation`.
The seven [TLS wire tests](../../../tests/tls.rs) pass:

| Test | Verified behavior |
| --- | --- |
| `tls_embedded_default_startup_exports_only_ca_and_keeps_custom_trust_separate` | Start with no certificate flags, export the public CA without config/data/network access, verify built-in trust, reject partial custom pairs and prove explicit custom CA replaces built-in trust in both directions. |
| `tls_trust_versions_streaming_urls_and_restart` | Default HTTPS startup, negotiated TLS 1.2/1.3, matching localhost SAN, 2 MiB+17-byte disk fixture equality, fixed HTTPS links despite hostile Host/Forwarded, HTTPS resumable Location/finalization, trusted probe and HTTPS-to-HTTP persistent restart. |
| `tls_rejects_untrusted_wrong_host_expired_and_plaintext` | Unknown CA, DNS mismatch, expired certificate and plaintext fail; a custom HTTPS listener rejects built-in/untrusted CA, mismatched host, query and invalid probe options. |
| `tls_handshake_admission_timeout_shutdown_and_strict_permissions` | One stalled handshake consumes the only admission slot; a second cannot finish until timeout releases it. Probe recovers; strict anonymous denial, admin success and viewer write denial; shutdown interrupts a five-second handshake wait in under two seconds. |
| `tls_invalid_material_never_reaches_readiness_or_creates_data` | Missing, empty, malformed, oversized and mismatched cert/key and duplicate keys fail without readiness, data-directory creation or private-key diagnostic output. |
| `tls_streaming_progress_and_upload_deadlines_preserve_existing_object` | Fragmented TLS upload succeeds; stalled/trickling uploads hit inactivity/absolute deadlines without replacing the existing resource or bytes. |
| `tls_healthcheck_deadline_includes_stalled_handshake` | A stalled TLS peer cannot hold the probe beyond its two-second budget. |

The [pre-push transcript](local-tls-pre-push.txt) records all 576 passing Rust
tests, the full HTTP Go SDK suite and both container architectures. The requested
`just pre-push` completed with exit code 0 before merging.
Use the official pinned toolchain, including its license documentation, on PATH:

```sh
toolchain_bin=$(dirname "$(rustup which --toolchain 1.99.1 rustc)")
PATH="$toolchain_bin:$PATH" cargo test --workspace --locked --offline
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

Formatting, workspace/all-target Clippy with warnings denied, compatibility
validation/generated-view checking, the two existing shell workflow self-checks,
all shell syntax checks and Markdown lint (ten changed documents, zero issues)
also pass. Markdown used the already installed markdownlint-cli2 0.24.0 binary;
no new package or lint-rule exception was added beyond the documented MD013
file directives for existing long prose/tables.

The TLS wire harness requires Bash, OpenSSL and curl on Unix and contacts only
loopback. Its CA/private keys are generated per test outside the checkout and
removed afterward. Existing portable HTTP foundation tests remain active.

## Official SDKs

- [Go HTTPS transcript](local-tls-go-https.txt): Go 1.27.3, Storage v1.70.0, generated API v0.308.0; vet and all 41 top-level tests pass against separate permissive/strict built-in HTTPS listeners, using automatically exported CA trust with hostname verification. The same full suite passes over explicit HTTP in the pre-push transcript. [Paired custom certificate workflows and missing cert/key/CA configuration failures](local-tls-go-custom.txt) also pass.
- [Ruby transcript](local-tls-ruby.txt): Ruby 4.0.9, Bundler 4.0.23, Storage 1.62.1; origin self-check and bucket get/list/update/delete, generated-client insert, 576 KiB disk-backed streaming, three resumable chunks, HTTPS Location origin, CRC32C and SHA-256 equality pass. High-level bucket insert's always-sent Autoclass remains unsupported; this is narrow TLS coverage, not full Ruby/Rails compatibility.

Commands used exported CA trust with verification enabled:

```sh
GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh
bash scripts/sdk-go.sh
target/debug/gcs-mimic tls ca > "$cert_dir/ca.pem"
cd sdk-tests/ruby
BUNDLE_FROZEN=true bundle install
bundle exec ruby https.rb --self-check
GCS_MIMIC_ENDPOINT=https://localhost:54447 GCS_MIMIC_CA_CERT="$cert_dir/ca.pem" \
  bundle exec ruby https.rb
```

No ADC, Google Cloud, metadata server or real-bucket request was made. Clients
disable proxies and redirects and guard the exact local origin.

## Native and container journeys

The runnable guide's [TLS section](../local-https.md#local-https-with-an-explicit-development-ca)
was exercised with the native binary, exported CA and `bash scripts/tls-smoke.sh`.
The built-in native listener was `0.0.0.0:54447`, advertised as
`https://localhost:54447`; browser peer access used `https://gcs:54447`.
Custom two-day certificates from `scripts/tls-fixtures.sh` separately exercise
the supplied-certificate path and negative trust/expiry cases.
The HTTP/HTTPS choice does not grant IAM access.

The requested pre-merge gate builds and exercises both final-source platforms:

```sh
just pre-push
```

[Build and smoke transcript](local-tls-pre-push.txt). Per-platform image digests are
recorded in that transcript; no registry publication is claimed.
ARM64 ran natively; AMD64 ran under Docker Desktop emulation. Each used UID/GID
10001, the read-only Compose root, read-only TLS mount, existing 128 MiB/CPU/PID/FD
bounds and its own data volume. Both passed config validation, `up --wait`,
explicit-CA healthcheck, bucket/media/XML/resumable smoke and stop/start probes.
Both architectures retained exact `builtin-restart` bytes across restart on
host port 4443 with the advertised origin `https://localhost:4443`. The default
image CMD and healthcheck passed without command overrides or certificate mounts.
ARM64 additionally passed the custom-certificate guide on host port 54444,
then followed an advertised `https://gcs:4443/upload/...` Location from the peer
curl container to a completed upload and exact `abc` download. The pre-push
HTTP regression script selects `compose.http.yaml` explicitly on both platforms.

The peer was pinned to `curlimages/curl:8.23.0` digest
`sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac`.
Container/network cleanup used `down` without resetting existing volumes.
The three isolated verification volumes were retained; no user's data volume
was removed. No image was published.

## Actual browser

Chromium 155.0.8059.79 on isolated Debian 12 verified
`https://gcs:54447/_internal/ready` and rendered the expected JSON with the
exported built-in CA trusted only in its NSS database. Removing that CA produced
`Page load failed: net::ERR_CERT_AUTHORITY_INVALID` and no response DOM.
[Browser check output](local-tls-browser.txt) records the verified trust results.

The temporary browser container installed Chromium and `libnss3-tools` from
Debian; it did not change host trust. Inside that container, the trust procedure
was the following, using paths relative to its working directory:

```sh
mkdir -p "$HOME/.pki/nssdb"
certutil -N -d "sql:$HOME/.pki/nssdb" --empty-password
certutil -A -d "sql:$HOME/.pki/nssdb" -n gcs-mimic-throwaway -t C,, -i ca.pem
chromium --headless --no-sandbox --disable-gpu --no-proxy-server \
  --dump-dom https://gcs:54447/_internal/ready
certutil -D -d "sql:$HOME/.pki/nssdb" -n gcs-mimic-throwaway
chromium --headless --no-sandbox --disable-gpu --no-proxy-server \
  --dump-dom https://gcs:54447/_internal/ready
```

`--no-sandbox` was restricted to the throwaway root browser container. No
certificate-error, hostname or TLS verification bypass was used. Cross-origin
CRUD and browser CORS remain the next slices.

## Inputs and limits

[Dependency verification](local-tls-dependencies.txt) reports all 35 checked
inputs CURRENT: Rust 1.99.1, all twelve direct Rust dependencies, official
Go/Ruby SDKs, Bundler, Actions and pinned container inputs. Exact versions,
licenses, package sizes, application/SDK lockfiles, Docker input digests and
daily detection are recorded in [dependency-status.json](../../../compatibility/dependency-status.json).
The new runtime dependency is Tokio Rustls with only ring and TLS 1.2 enabled;
no second crypto provider or PEM parsing dependency is added. The detector now
uses Cargo's actual direct resolution edges when multiple transitive versions
share a name, and checks Ruby/Bundler/gems on every scheduled run.

Public issuance, system trust changes, dual listeners, mTLS, reload, CORS,
HTTP/2, HTTP/3 and gRPC were not implemented. Actual GCS supports all three HTTP
versions; its gRPC API is separate. Source-only fake-gcs-server TLS findings are
recorded in the contract; its TLS runtime was not compared. No real-cloud,
power-loss, performance, native AMD64 or remote CI claim is made.
