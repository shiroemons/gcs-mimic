# Ruby SDK defaults

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-083, JSON-OBJECTS-007, JSON-BUCKETS-005 and JSON-OBJECTS-002.
No storage, startup, authentication or dependency changes are required.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| RUBY-DEFAULT-CREATE-FILE-CONTENT-TYPE | High-level `create_file(StringIO, extensionless_name)` without `content_type` accepts the SDK's empty X-Upload-Content-Type as absent; the existing absent-header fallback remains application/octet-stream. Nonempty invalid MIME values still return 400. |
| RUBY-DEFAULT-CREATE-BUCKET | High-level `create_bucket(name)` without optional arguments accepts autoclass enabled:false as absent. Empty/null Autoclass settings are also absent. Any other Autoclass meaning returns 501 notImplemented, and responses never advertise Autoclass. |
| RUBY-DEFAULT-COMPOSE | High-level `compose(sources, name)` without optional arguments accepts deleteSourceObjects:null as false and destination:null as an omitted destination. Sources survive, bytes concatenate and existing destination defaults apply. True retains hard deletion; malformed and unsupported inputs retain their existing errors. |
| RUBY-DEFAULT-SUITE | The pinned Ruby suite uses these high-level default calls, including migration create_file, without the three workarounds. Existing checksum, cleanup and origin guards remain enabled. |

The JSON adapter normalizes only these no-op defaults. Enabled Autoclass,
additional Autoclass properties and unsupported locations still fail rather than
silently succeeding. Bucket PATCH/PUT Autoclass support is outside this slice.
Duplicate upload headers remain invalid. Body streaming, publication conditions
and strict permission checks use their existing paths.

## Sources and local decisions

Checked 2026-10-27 against the [official compose method](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose),
the [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets)
and [Storage v1 Discovery](https://storage.googleapis.com/$discovery/rest?version=v1).
Discovery revision: `20261020`; SHA-256:
`eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.
Discovery defines the optional boolean and destination object, and the compose
method documents false as the deletion default. Neither establishes explicit-null
acceptance for this legacy JSON API. Treating these nulls as absent is a local
SDK compatibility decision recorded in SPEC-RUBY-DEFAULTS, not provider evidence.
Empty upload MIME and disabled/empty Autoclass are likewise normalized only at
the adapter boundary; Autoclass remains unimplemented.
This supersedes the explicit-null rejection in the original
[compose contract's request table](object-compose.md#supported-request-and-metadata-subset).
An omitted/null destination still omits contentType from JSON metadata and serves
application/octet-stream on download; no new metadata inheritance is introduced.

Inputs remain pinned to Ruby 4.0.9, Bundler 4.0.23, google-cloud-storage 1.62.1,
Go 1.27.3 and cloud.google.com/go/storage v1.70.0.

## Verification

Acceptance was recorded before implementation. On 2026-10-27, the unmodified
server failed all three new wire tests: bucket creation returned 501; empty MIME
and null compose returned 400. The pinned Ruby high-level calls independently
reproduced the same failures before the adapter changes.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test --locked --offline --test sdk_ruby_defaults_http --test compose_validation --test bucket_http`: PASS (3 default cases, 6 compose validation cases, 63 bucket cases).
- `cargo test --locked --offline --test object_compose_http`: PASS (18 existing compose HTTP cases, executed together with the first targeted run).
- `cargo test --locked --offline --lib json::tests::bkt_ins_002_recognized_unsupported_fields_never_succeed`: PASS.
- `BUNDLE_FROZEN=true bundle check` with pinned Ruby/Bundler: PASS.
- `just check-sdk` (HTTP, one final-source execution): PASS. Go vet and the complete Go suite pass, including TestSDKObjectDefaults and TestBucketLifecycle. Ruby origin self-check, migration scenario and basic suite pass; all three default-case markers appear in permissive and strict runs. Exact downloads and SDK checksum checks pass. Generated PUT/copy and the signed URL endpoint adaptation remain necessary and are documented in the Ruby README and DESIGN-083 deviations.
- `just check` (one final-source execution): PASS, including formatting, host/cross-target clippy, the complete Rust test suite, compatibility validation/generated views and offline script checks.
- `npx --yes markdownlint-cli2 docs/en/slices/sdk-ruby-defaults.md docs/en/fake-gcs-server-migration.md sdk-tests/ruby/README.md planning/onboarding-tracks-plan.ja.md`: PASS (0 issues).

The optional Japanese `tanteki` textlint check exits 1 for the whole existing plan:
120 findings are outside the new SDK section; its 12 findings concern the
requested term `スライス` and the 61-character feature-ID cell. Those specified
terms/IDs are retained; this is not a claim that the whole plan passes textlint.

The slice-start `cargo xtask dependencies check` exited 1: Rust 1.99.1, Ruby,
Bundler and google-cloud-storage were current; google.golang.org/api v0.309.0
and a BuildKit SBOM scanner digest update were available. Those independent
dependency updates are outside this slice; no dependency or lockfile changed.

HTTPS, containers, full guide journeys and benchmarks are excluded because this
slice changes JSON defaults only; it does not change TLS, images or guide commands.
Real Google Cloud differential tests are not run.
