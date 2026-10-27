# Bucket permission tests

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: JSON-BUCKETS-012 and DESIGN-028. Object permission tests and extensions to
ACLs, UBLA or PAP are excluded. Existing authorization semantics remain unchanged.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| TEST-PERMISSIONS-ROUTE | GET bucket iam/testPermissions accepts repeated permissions and returns storage#testIamPermissionsResponse with permissions. Missing/invalid names and project-only bucket create/list fail with 400; absent buckets fail with 404. |
| TEST-PERMISSIONS-STRICT-EFFECTIVE | The same authorize_bucket/catalog bucket_access function used by operations checks each supported permission; project and bucket grants are additive. Mixed grants, denial without policy-read permission and revocation are checked. |
| TEST-PERMISSIONS-PERMISSIVE | All supported requested permissions are returned because permissive operations do not authorize callers. This is a local profile choice. |
| TEST-PERMISSIONS-SDK | Default Go IAM().TestPermissions and Ruby bucket.test_permissions calls work in strict and permissive modes. |

No permission is required to invoke this method. Strict credential validation
still applies. Supported names are the existing operation permissions except
project-only storage.buckets.create and storage.buckets.list. Other known Storage
permissions return 501; unknown names return 400. Empty userProject is absent;
nonempty userProject returns 501 because Requester Pays is unsupported.

## Sources and local decisions

Checked 2026-10-27: [method reference](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/testIamPermissions),
[permission catalog](https://docs.cloud.google.com/storage/docs/access-control/iam-permissions)
and [Storage v1 Discovery](https://storage.googleapis.com/$discovery/rest?version=v1).
Discovery revision `20261020`, SHA-256
`eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.
The method documents no required permissions, a required permissions query and
errors for project-only bucket permissions. Error envelopes for missing input,
unknown names and absent buckets are local choices pending provider evidence,
recorded in SPEC-TEST-PERMISSIONS. No real-cloud requests are authorized.

The adapter reuses `server::authorize_bucket` and `Catalog::bucket_access`;
no authorization logic or storage interfaces are added. Each permission uses
an existing short transaction, so the response is advisory rather than an atomic
snapshot during policy changes. Duplicate requested permissions are returned once,
in request order. Error precedence and this ordering are local choices.

JSON-BUCKETS-012 and DESIGN-028 remain PARTIAL with LOCAL_SIMULATION fidelity.
Other permissions, Requester Pays, object testIamPermissions, expanded
ACL/UBLA/PAP interactions and provider error parity remain pending.

## Verification

Acceptance was recorded before implementation. The unmodified server returned
404 for the default Go strict/permissive and Ruby high-level calls. Implementation
and tests are in `c185ea0`; dependencies and lockfiles are unchanged.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test --locked --offline --test auth_http test_permissions`: PASS, 2 wire tests covering mixed grants/revocation, anonymous and unprivileged callers, invalid credentials, request validation and fields.
- `cargo test --locked --offline --test auth_http`: PASS, all 104 tests, including existing IAM route validation.
- `BUNDLE_FROZEN=true bundle check` with pinned Ruby: PASS.
- `just check-sdk`: PASS, one final-source run; Go vet/full suite and Ruby self-check/migration/basic suites, including strict/permissive permission tests.
- `just check`: PASS, one final-source run; formatting, host/Windows/Linux-musl Clippy, all workspace tests, generated ledger checks and offline shell/release checks.
- Markdown lint: PASS for the English/Japanese slice pages and migration table. Japanese textlint: PASS with the two feature IDs allowed as specification identifiers rather than Jira references.

Inputs: Rust 1.99.1, Go 1.27.3, Storage Go v1.70.0/API v0.308.0,
Ruby 4.0.9, Bundler 4.0.23 and google-cloud-storage 1.62.1.
The slice-start dependency check found API v0.309.0 and a new BuildKit SBOM
scanner digest; all other inputs were current. Those independent updates are
outside this slice.

Containers: not run because they are outside this slice.
Full guide verification is outside this slice because guide commands are unchanged.
HTTPS SDK and real-cloud differential tests are not run. Benchmarks are excluded.
