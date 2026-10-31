# Strict local service-account JWTs and Rails signing

<!-- markdownlint-configure-file {"MD013": false} -->

This scope extends DESIGN-002, DESIGN-010, DESIGN-083 and DESIGN-084 with local
RSA bearer authentication. It preserves the existing strict IAM decisions,
opaque tokens, V2/V4 signature verification and permissive profile. It is a
local identity simulation, not Google OAuth verification or token issuance.
Broad authentication and Rails feature rows remain PARTIAL.

## Pinned contract

Sources checked on 2026-10-30:

- Google's [token types](https://docs.cloud.google.com/docs/authentication/token-types) distinguishes a service-account JWT from an OAuth assertion: a scoped JWT omits `aud`, has the service-account email in both `iss` and `sub`, and carries `iat` and `exp`. An assertion for exchange at `https://oauth2.googleapis.com/token` is a different credential.
- [googleauth 1.17.6, commit bee27e1c2ee1133ae3ed03a2e5461c677e35a2ab](https://github.com/googleapis/google-auth-library-ruby/blob/bee27e1c2ee1133ae3ed03a2e5461c677e35a2ab/lib/googleauth/service_account_jwt_header.rb) signs RS256, emits matching `iss`/`sub`, defaults `iat` to 60 seconds before now and `exp` to 60 seconds after now, and omits `aud` when a scope is supplied. Source SHA-256: `3ab72ab5036bc15921f8c2952afb61189395a2514753f199a0a577deb5b16dfe`.
- [Ruby Storage 1.62.1, commit 76a3362dda291957f9979addb01329babc485ad8](https://github.com/googleapis/google-cloud-ruby/blob/76a3362dda291957f9979addb01329babc485ad8/google-cloud-storage/lib/google/cloud/storage/file/signer_v4.rb) uses `storage.googleapis.com` as the default V4 canonical Host even with an endpoint override. Its public `bucket_bound_hostname` option signs the supplied host and port but omits the bucket path. Source SHA-256: `392dea55133add91c6717de70b47f572287d34ddd8be37d199a9cb256fc596fb`.
- [Rails Active Storage 8.1.4, commit c3466ea00d7121798e3aa3144ffdf7174b81d8cb](https://github.com/rails/rails/blob/c3466ea00d7121798e3aa3144ffdf7174b81d8cb/activestorage/lib/active_storage/service/gcs_service.rb) normally uses V2 direct uploads and selects V4 when `cache_control` is present. Source SHA-256: `e24512c9ee72e009a6aeb953857fde3d2c8f3372656c3241d33caea43faec59b`.
- [Google canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) requires signing the actual final Host, URI and required headers. The verifier retains this requirement.
- The migration reference is fake-gcs-server v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`; its permissive behavior is not an authentication specification.

## Accepted bearer subset

The strict identity file remains the existing bounded JSON format. A JWT's `iss`
must name an existing identity whose `public_keys` contains its RSA public key.
Only those startup-loaded public keys are trusted. No `jku`, `x5u`, discovery,
metadata-server or token-exchange request is made.

The supported JWT is three canonical, unpadded base64url segments, at most
8192 bytes total. Its header requires `alg: RS256`; optional `typ` must equal
`JWT`, and an optional nonempty `kid` of at most 256 bytes is informational.
The bounded list of registered keys is tried regardless of `kid`; key IDs do
not add keys or choose another principal. Other header fields are rejected.

Claims require:

- `iss` and `sub` equal the same registered identity ID.
- `scope` equals exactly `https://www.googleapis.com/auth/devstorage.full_control`.
- No `aud` field, including null. Audience JWTs, Google ID tokens and OAuth exchange assertions are unsupported and rejected.
- Integer Unix-second `iat <= now < exp`, with `0 < exp - iat <= 3600`. There is no extra clock-skew grace. Optional `nbf` cannot exceed now or reach expiry.
- No other claim fields or duplicate known header/claim fields.

Read-only, read-write, cloud-platform and mixed scopes are rejected, including
when the signer is an admin. They are not silently promoted to full control.
Adding narrower scopes requires carrying a scope permission cap through every
authorization path. This slice does not add that separate capability.

After successful verification, existing project grants, current bucket policy,
public principal sets, PAP, overwrite rules and transactional authorization
apply to the same registered identity. A signature grants no permission itself.
Invalid credentials do not become anonymous even when the object is public.
JWT-shaped entries in `tokens` cannot bypass cryptographic verification.
Other registered opaque tokens retain their behavior.

Every production bearer path uses the server's injected clock, including JSON,
XML, forms, compressed requests, admin routes and both gRPC adapters. The
SystemClock convenience wrapper exists only in tests. Registry keys and tokens
are loaded once before readiness; file changes take effect after restart.
Rotate by registering both public keys, restarting, switching the client, then
removing the old key and restarting. Restart also preserves stored objects.

## Rails configuration and final browser URLs

Use `universe_domain: gcs-emulator.invalid` in both the Rails service and its
local service-account credential hash. This makes the pinned googleauth sign
its bearer token without contacting Google. Register `client_email` as the
identity ID, register its public key, and grant only the needed local project or
bucket roles. The server receives no private key. Never use production keys.

The ordinary `service: GCS` entry with no `cache_control` supports stock Rails
V2 signed downloads and direct uploads. Its endpoint must be reachable by the
requesting client. `endpoint` does not automatically fix Ruby's V4 canonical
Host. The verifier does not accept a Google Host signature at a local Host.

The explicit [EmulatorGcs service](../../../sdk-tests/ruby/active_storage/lib/active_storage/service/emulator_gcs_service.rb)
uses the SDK's public signing API for final-origin V4 GET/PUT URLs. It prepends
the bucket before signing in bucket-bound mode, and preserves checksums,
Cache-Control, custom metadata and response queries. Copy this adapter into an
application's `lib/active_storage/service` directory and make `lib` available on
the load path. Select `service: EmulatorGcs` explicitly for this local profile.
It requires local signing credentials and rejects IAM remote signing.

`endpoint` is the server-to-server API origin; `public_endpoint` is a bare final
HTTP(S) origin reachable by the browser. For example, a Compose peer can use
`endpoint: http://gcs:4443/` while its host browser uses
`public_endpoint: http://localhost:4443/`. The URL is generated for that public
authority before signing. Never replace its hostname after signing. This
adapter's private signed flows do not configure DNS, proxying, TLS trust or
anonymous public delivery. Use a reachable common endpoint or Rails proxy mode
for stock service URLs that expose a container-only hostname.

Direct upload CORS remains bucket configuration. Allow the actual Rails origin,
PUT, and `Content-MD5`, `Content-Disposition`, `Content-Type` and `Cache-Control`
as needed. Successful OPTIONS never grants upload permission. CORS, Host
routing, network reachability, PAP and authentication remain separate checks.

## Acceptance and verification

`auth::jwt::tests` covers valid local signatures, opaque-token compatibility,
scope restrictions, duplicate claims, algorithms, issuer/subject, audiences,
expiry/not-before/lifetime boundaries, canonical base64url, tampering,
registered unsigned-token bypass and key overlap/removal.

`signing::jwt` in `auth_http` covers JSON upload/download and XML bearer reads,
IAM denials and unchanged objects, private/public/allAuthenticatedUsers behavior,
PAP with explicit bearer and V4 signer grants, browser-equivalent direct-upload
preflight headers, and key removal/re-registration across restart in both
native and legacy profiles.

The isolated Rails scenario uses the existing pinned gem graph, an in-memory
Rails database and only loopback Net::HTTP connections. It exercises ordinary
strict upload/download, checksum-verified open, stock V2 signed GET/PUT,
the explicit adapter's V4 signed GET/PUT, CORS allowed/denied origins,
PAP, tampered query rejection and cleanup. Run it against the final built source:

```sh
cargo build --locked
bash sdk-tests/ruby/active_storage/run-strict.sh
```

The strict scenario passes against the integrated native runtime as recorded
below. Browser-equivalent HTTP
requests are not actual browser enforcement or container reachability evidence.
The existing permissive `attachments.rb` recipe remains a separate regression
target. No benchmark or performance improvement is claimed.

Local verification on 2026-10-30:

| Command | Result |
| --- | --- |
| `cargo check --tests` | PASS |
| `cargo test --lib auth::` | PASS, 28 tests |
| `cargo test --test auth_http --test xml_storage_http` | PASS, 121 authentication/route tests and 13 XML LIST/COPY tests, including all three JWT wire cases |
| `cargo xtask lint` | PASS, zero production violations |
| `bundle exec ruby signing_self_check.rb` in the Active Storage suite | PASS, independent RSA verification binds final host/port/path/checksum and rejects changed port/checksum without network access |
| `ruby -c` on the adapter, strict scenario and signing self-check | PASS |
| `bash -n sdk-tests/ruby/active_storage/run-strict.sh` | PASS |
| `bash sdk-tests/ruby/active_storage/run-strict.sh` with the pinned Ruby 4.0.9 gem graph | PASS: signing self-check and three runtime scenarios (`AS-STRICT-JWT-UPLOAD-DOWNLOAD`, `AS-STRICT-V2-DIRECT-UPLOAD-CORS`, `AS-STRICT-V4-DIRECT-UPLOAD-CORS`). No authorization removal; private unsigned reads, PAP and changed signed-header rejection remain asserted. |
| `npx --yes markdownlint-cli2 docs/en/slices/strict-service-account-jwt.md sdk-tests/ruby/active_storage/README.md` | PASS, zero issues |

The strict Rails run initializes the configured Active Storage service through
its public accessor before reading its lazily loaded service configuration.
Both YAML entries have their required fixture environment settings. The child
Rust server excludes the runner-only binary-path environment setting; ordinary
unknown native settings remain rejected. These are fixture setup corrections,
not authentication or signature workarounds.

Actual Rails browser UI and container reachability remain unexecuted in this
scope. The permissive recipe's historical browser evidence is separate from
this strict native scenario. [Combined evidence](../evidence/migration-integration.md)
records the final integration boundaries.
