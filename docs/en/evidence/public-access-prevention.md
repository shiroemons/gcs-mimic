# Public access prevention verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified **2026-10-23**, code commit
`4611758c7d7047289c7426698e53a83d94ee1ff8` on
`shiroemons/public-access-prevention`.
The [slice contract](../slices/public-access-prevention.md) records source hashes,
acceptance cases, parent-policy fixture and exact guide integration commands.

| Command | Result | Raw evidence |
| --- | --- | --- |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --offline --workspace` | PASS, 635 tests; zero failed/ignored | [Rust](public-access-prevention-rust.txt) |
| `env -u RUSTUP_TOOLCHAIN cargo fmt --all --check` | PASS | No output, exit 0 |
| `env -u RUSTUP_TOOLCHAIN cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS | [Clippy](public-access-prevention-clippy.txt) |
| `GCS_MIMIC_GO_TEST_RUN=TestPublicAccessPrevention bash scripts/sdk-go.sh` | PASS, Go vet and targeted Go test over HTTP | [Go HTTP](public-access-prevention-sdk.txt) |
| `GCS_MIMIC_SDK_SCHEME=https GCS_MIMIC_GO_TEST_RUN=TestPublicAccessPrevention bash scripts/sdk-go.sh` | PASS, Go vet and targeted Go test with trusted local CA | [Go HTTPS](public-access-prevention-sdk-https.txt) |
| `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` | PASS, all 36 inputs CURRENT | [Freshness](public-access-prevention-dependencies.txt) |
| `env -u RUSTUP_TOOLCHAIN cargo xtask compatibility render` and `check` | PASS, 591 feature rows; English/Japanese matrices regenerated | Ledger and generated views |
| `npx --yes markdownlint-cli2 docs/en/slices/public-access-prevention.md docs/en/evidence/public-access-prevention.md` | PASS, zero issues | Existing MD013 exception for prose/tables; other rules remain enabled |

The raw logs retain actual output with only machine-specific path prefixes
redacted. Expected TLS rejection/timeout messages in the Rust log are emitted by
negative tests; the final suite has no failures or ignored cases. Earlier
iteration failures were fixed before this final evidence set.
The Rust transcript's final blank line is preserved; source whitespace checks
exclude that raw log.

## Verified scope

- Both bucket compatibility profiles persist/render inherited/enforced PAP on
  insert/PATCH/PUT/GET/list/fields, with atomic conditions, revision changes and
  restart. Schema 12 upgrades to 13; the older-schema migration corpus also
  passes without changing object history, IAM or bytes.
- Strict anonymous and registered callers relying on either public principal
  set lose JSON/XML read/write/list/delete permission under effective PAP.
  Existing bindings and etag remain readable and unchanged. Public policy
  submissions fail 412; removing public members and explicit nonpublic grants
  work. Disabling PAP reactivates stored public grants.
- The bounded startup parent fixture enforces inherited buckets across restart,
  does not affect other projects and cannot be bypassed by setting inherited.
  Removing the fixture reactivates inherited public access; explicit bucket
  enforcement persists. This infrastructure fixture is LOCAL_SIMULATION.
- Valid V4 RSA GET/HEAD/PUT still uses nonpublic signer grants; creator/overwrite
  requirements remain enforced. A signer relying only on a public grant is
  denied. Invalid signatures never fall back or replace existing bytes/metadata.
  Bearer project/user grants continue to work, and public cache classification
  observes PAP.
- Post-admission enforcement is rechecked before shared reads/mutations and
  upload publication, with unchanged live bytes and cleaned staging files.
  Both metadata mutation methods require update plus setIamPolicy permission.

Go 1.27.3 / storage v1.70.0 / API v0.308.0 verifies
`BucketAttrs.PublicAccessPrevention` on create/get/update, public denial,
preserved policy, 412 rejection, signed GET/PUT and reactivation. The unmodified
official signer uses a final default-port URL through an origin-guarded local
dialer, matching the existing SPEC-GO-SIGNER-PORT contract. The signed URL is
never rewritten after signing. HTTP and verified-CA HTTPS both pass.

## Ledger and limits

BEHAVIOR-BUCKET-014 and DESIGN-022 describe the requested effective-parent
fixture subset with target/actual fidelity LOCAL_SIMULATION. DESIGN-023 and
DESIGN-025 cover their scoped documented authorization behavior. The affected
bucket metadata/IAM method rows retain PARTIAL status and existing broader
pending cases. SPEC-PAP-EDGES preserves the local null/legacy/PUT/error choices;
ACLs, UBLA, full organization policy and propagation are not implemented.
DESIGN-024 is PARTIAL: scoped IAM 412 rejection passes, while ACL creation
exceptions and exact provider reason/message/precedence remain pending.
DESIGN-012/013 and UX-005 retain their status; their former combined unsupported
PAP/uniform-access test is now `xml::uniform_access_settings_fail_closed`, with
real PAP enforcement covered by the new acceptance cases and current evidence.

UX-006 remains PARTIAL: Rust/Go PAP API behavior is verified and runnable
commands are supplied, but the final guide addition/native/container journey
has not been executed here. No container build, guide walkthrough, Ruby run,
actual browser, Google request, release/push, power-loss or new performance/RSS
claim is made. Permissive authentication remains explicitly unauthenticated.
Final integration must copy/run the guide PAP block, rerun the combined gates
on the final source, and decide the UX-006 completion status.
