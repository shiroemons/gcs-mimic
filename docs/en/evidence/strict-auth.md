# Strict local authentication verification

Contract: [strict local authentication and public URL helper](../slices/strict-auth.md).
Sources checked on **2026-10-22** (pages last updated 2026-10-21 UTC). Rust 1.99.1 /
edition 2024; Go 1.27.3, Storage v1.69.0 and API v0.307.0 use the committed module
graph. No real Cloud Storage, Google identity or metadata-server request is made by
any of the commands below.

## Integrated behavior

`auth_mode=permissive` remains the default and checks nothing. `auth_mode=strict`
loads a bounded JSON identity registry at startup, resolves callers from local
bearer tokens and enforces the iam-json permission of each supported JSON route
against project-level predefined storage roles. Anonymous callers receive 401
`required`; unknown or unsupported credentials, including credential query
parameters, receive 401 `authError`; authenticated callers lacking a permission
receive 403 `forbidden`. Overwriting a live object also needs
`storage.objects.delete`, enforced before the body is read and rechecked in the
publication transaction. Every strict catalog operation rechecks the authorized
bucket project in its own transaction, so a bucket recreated in another project
yields 403 without mutation. `gcs-mimic url public` builds an encoded XML-style
path URL without reading configuration or changing state; that URL is not served
until the next slice.

## Ledger decisions

| Row | Final status | Achieved fidelity | Reason |
| --- | --- | --- | --- |
| DESIGN-001, explicit permissive profile | IMPLEMENTED | PROJECT | Default, selection, precedence and pairing errors pass |
| DESIGN-003, local principal registry | IMPLEMENTED | PROJECT | Validation, resolution, restart, Go fixture and Compose cases pass |
| DESIGN-008, predefined role expansion | IMPLEMENTED | EXACT | Dated 20-role `storage.*` snapshot evaluated as data; scope excludes non-storage permissions and basic roles |
| DESIGN-046, public URL construction | IMPLEMENTED | PROJECT | Pure CLI helper with encoding/validation tests and guide check |
| DESIGN-004, anonymous versus authenticated callers | PARTIAL | LOCAL_SIMULATION | SPEC-017 error details and missing-bucket rule are local; `allUsers` is not modeled |
| DESIGN-002, strict profile | PARTIAL | LOCAL_SIMULATION | Project roles only; IAM policies, ACLs, public grants, signatures and HMAC remain |
| DESIGN-082, official Go SDK | PARTIAL | PROJECT | Strict suite added; resumable, signing and broader workloads remain |
| JSON-BUCKETS-001/002/005/006, JSON-OBJECTS-004/005/007/008/010/015 | PARTIAL | LOCAL_SIMULATION | Strict project-role cases added; IAM/ACL and other method scope remain |

DESIGN-004 targets EXACT, and IMPLEMENTED requires achieved fidelity equal to the
target with no deviations. Its open SPEC-017 deviations therefore keep it PARTIAL.
DESIGN-008's EXACT claim covers only the dated snapshot: the transcription was
reviewed against the source page, and no automated comparison with a pinned source
artifact exists. Implemented rows record commit
`1153d399dfb858d59e7c815150a02a6ae3c92ea7`; DESIGN-046 records the helper commit
`4aea4984eaf91894b417be0438433fc396ad37b6`.

## Final verification

Each log below was produced once on 2026-10-22 from the final implementation
source, commit `1153d399dfb858d59e7c815150a02a6ae3c92ea7`. Later commits change
only documentation, the ledger and evidence. The guide transcript ran on the
documentation that contains the strict section. Local path prefixes in the logs
were redacted; all other output is unchanged.

| Command or workflow | Result | Evidence |
| --- | --- | --- |
| `cargo +1.99.1 test --workspace --locked --offline` | PASS: 223 tests, 0 failed | [Test log](strict-auth-tests.txt) |
| `cargo +1.99.1 fmt --all --check`, `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings`, `bash scripts/test-check.sh`, `cargo +1.99.1 xtask compatibility check` | PASS | [Checks log](strict-auth-checks.txt) |
| `bash scripts/sdk-go.sh` (permissive and strict native servers) | PASS: 43 Go test/subtest results, 0 failed | [Go SDK log](strict-auth-sdk-go.txt) |
| Every `sh` block in `docs/en/getting-started.md`, in order under Bash `-euxo pipefail` | PASS (exit 0) | [Guide transcript](strict-auth-guide.txt) |
| `bash scripts/smoke-containers.sh` (linux/arm64 native, linux/amd64 emulated) | PASS | [Container log](strict-auth-containers.txt) |
| Slice-start `just check-dependencies` | CURRENT after adopting the updated Rust builder index | [Freshness log](strict-auth-dependencies.txt) |

Native amd64 hosts, remote CI and other SDKs were not run for this slice.

The guide's strict section runs the Compose service with `compose.strict.yaml`,
checks the ready event, exercises anonymous, unregistered, admin, viewer and
creator callers, runs the strict Go suite against Compose and confirms that the
public URL helper output is not served. Ledger test IDs
`guide-compose-strict-auth` and `guide-compose-go-strict-suite` refer to that
transcript.

## Limits and next slice

SPEC-017 keeps the `authError` message/location, the short 403 message, the
absent `WWW-Authenticate` header, the missing-bucket 404/403 rule, the absence of
extra permissions for `projection=full` and the 412-before-403 overwrite order
open until authorized real-service evidence exists. Tokens are not constant-time
compared, and duplicate registry keys follow last-value-wins JSON parsing.

Bucket/object IAM policies, ACLs, `allUsers`/`allAuthenticatedUsers`, UBLA, public
access prevention, signed URLs, HMAC, service-account signing, XML endpoints,
Ruby/Rails, native amd64, remote CI and real-service differential tests remain
unverified or unimplemented. The next slice adds bucket IAM policy get/set,
`allUsers`/`allAuthenticatedUsers` grants and XML path-style GET/HEAD for public
reads.
