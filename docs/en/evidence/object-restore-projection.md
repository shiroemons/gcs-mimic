# Object restore projection

Scope: JSON-OBJECTS-011 (`objects.restore`) response projection.

## Contract sources

- The [objects.restore reference](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/restore),
  checked 2026-10-31, documents `projection` as "Set of properties to return.
  Defaults to `noAcl`" with the values `full` ("Include all properties") and
  `noAcl` ("Omit the `owner`, `acl` property"). It documents `copySourceAcl`
  default `false`: the restored object inherits the bucket default object ACL.
- The Java SDK `google-cloud-storage` 2.76.0 `HttpStorageRpc.restore` always
  sends `projection=full` (`DEFAULT_PROJECTION = "full"`). gcs-mimic rejected
  that request with a 400 unsupported-setting error.

## Real Cloud Storage observation

Observed 2026-10-31 with curl against the production JSON API from an opt-in
test project, using a private, uniquely named bucket that was deleted
afterwards. Project, bucket and principal names are omitted.

On a bucket with fine-grained access control, an object whose insert response
contained `acl` and `owner` was soft-deleted and then restored:

| Restore request | Status | `acl` in response | `owner` in response |
| --- | --- | --- | --- |
| `projection=full` | 200 | absent | absent |
| `projection=noAcl` | 200 | absent | absent |
| projection omitted | 200 | absent | absent |

The provider therefore accepts `projection=full` on restore but renders the
response like `noAcl`. The ACL that a later `objects.get?projection=full`
returns for the restored generation was not observed.

## Local behavior

- `projection` is validated (`full` or `noAcl`) and then has no effect on the
  restore response, which never contains `acl` or `owner`, on fine-grained and
  uniform bucket-level access buckets alike.
- The stored ACL of the restored generation is unchanged: it is the bucket
  default object ACL (with the preserved owner), following the documented
  `copySourceAcl=false` default, and `objects.get?projection=full` renders it
  as before. Under uniform bucket-level access that read omits the ACL.
- `copySourceAcl=true` and `restoreToken` remain rejected.

## Verification

- Before the fix, the integration test's `projection=full` restore returned
  400 `badRequest` with the unsupported-setting message.
- `cargo test --locked --test acl_permission_effects restore`: PASS, including
  `restore_accepts_projection_full_and_omits_acl_and_owner_from_the_response`
  on the disk and memory backends.
- `cargo test --locked --lib soft_delete`: PASS (nine cases).
