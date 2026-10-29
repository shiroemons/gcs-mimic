# JSON access control list verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified **2026-10-29** on the ACL track branch. The
[slice contract](../slices/json-acl.md) records the acceptance cases, local
choices and limitations.

| Command | Result | Raw evidence |
| --- | --- | --- |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --offline --test acl_http --test unsupported_http` and `cargo test --locked --offline --lib -- acl:: unsupported::` | PASS, 8 + 1 HTTP tests and 8 unit tests | [ACL tests](json-acl-tests.txt) |
| `bash scripts/test-rust.sh` | PASS for every binary; `bucket_http` hit `AddrNotAvailable` (ephemeral ports exhausted by concurrent local test runs) and passed 69/69 when rerun alone | Not retained |
| `env -u RUSTUP_TOOLCHAIN cargo fmt --all --check` | PASS | No output, exit 0 |
| `env -u RUSTUP_TOOLCHAIN cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | No warnings |
| `bash sdk-scorecard/run.sh --out "$TMPDIR/scorecard-acl" gcs-mimic` | PASS for `bucket.acl`, `bucket.default_object_acl` and `object.acl` with Go storage v1.70.0 and Ruby google-cloud-storage 1.62.1; no other case changed from the 2026-10-29 baseline (score 78.7 to 84.3) | Scorecard output not committed |
| `env -u RUSTUP_TOOLCHAIN cargo xtask compatibility render` and `check` | PASS | Ledger and generated views |

## Verified scope

- Bucket, default object and object ACL list/get/insert/patch/update/delete,
  `projectPrivate` defaults, Discovery resource fields and etags, metageneration
  advance, 100-entry limit, invalid entity/role/body/query rejection without
  mutation, missing bucket/object/generation 404 and restart persistence
  (`bucket_acl_crud_round_trips_and_advances_metageneration`,
  `acl_requests_reject_invalid_inputs_without_mutation`,
  `object_acl_targets_a_generation_and_renders_with_full_projection`,
  `acl_entry_limit_is_enforced`).
- Default object ACL inheritance only for later generations, including
  replacement generations, and default list metageneration conditions
  (`default_object_acl_applies_only_to_objects_created_afterwards`).
- Full projection on bucket/object get/list/patch/update and copy, and echoed
  ACL acceptance versus changed ACL rejection in metadata PATCH/PUT
  (`object_acl_targets_a_generation_and_renders_with_full_projection`,
  `metadata_updates_accept_only_echoed_acls`, updated bucket and object HTTP
  suites).
- Uniform access rejection with retained entries, public access prevention
  rejection of public entities, and strict IAM permission checks
  (`uniform_access_and_public_access_prevention_restrict_acls`,
  `strict_profile_requires_iam_policy_permissions`).

No real Cloud Storage request was made; provider error text, upsert semantics
and the other local choices in the contract remain unverified.
