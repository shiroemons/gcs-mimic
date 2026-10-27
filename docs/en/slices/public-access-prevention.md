# Public access prevention and signed access

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-23**. Scope: BEHAVIOR-BUCKET-014, DESIGN-022,
DESIGN-023 and DESIGN-025, contributing metadata/IAM cases for JSON-BUCKETS and
the PAP case of UX-006. The runnable guide and its final execution belong to
integration; this slice supplies the commands below. The ledger owns status.

## Sources and reconciliation

The official [PAP overview](https://docs.cloud.google.com/storage/docs/public-access-prevention)
defines inheritance, preserved-but-overridden public grants, rejection of public
policy additions at HTTP 412, reactivation after disabling and signed access.
The [management guide](https://docs.cloud.google.com/storage/docs/using-public-access-prevention)
defines bucket administration and the [organization constraint](https://docs.cloud.google.com/storage/docs/org-policy-constraints)
defines `constraints/storage.publicAccessPrevention` as a boolean constraint.
The [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets)
accepts `inherited` and `enforced`, defaulting to `inherited`.
[PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch)
and [PUT](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/update)
require `storage.buckets.update`, adding `storage.buckets.setIamPolicy` for PAP
changes. The local comparison uses the resulting configured value: resending
the stored value or omitting PAP requires only update. PUT preserves omitted PAP
under the local SDK compatibility choice in SPEC-BUCKET-PUT. Unchanged-value provider
permission parity has not been measured; this interpretation is recorded in
SPEC-PAP-EDGES. Insert retains its project `storage.buckets.create` check.

The official Python `google-cloud-storage` **3.4.1**
[`IAMConfiguration` constructor and setter](https://github.com/googleapis/python-storage/blob/5ee391db2b2a9ffe92113359333151803acea259/google/cloud/storage/bucket.py#L475-L559)
populate `uniformBucketLevelAccess: {"enabled": false}` and send the whole
IAM configuration when PAP changes. The pinned `bucket.py` SHA256 is
`107fffb144c6dc0615663297367235dcca18e88d8414fb80f2438fe4eb79704b`.
The wire case reproduces that body exactly; no Python runtime/SDK execution is
claimed and this historical source pin is not a latest-SDK claim.

The existing Discovery revision `20261013`, SHA256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`, and
googleapis commit `14a161a98ef045be80c12ab69d924504a756f109` remain pinned in
the [source manifest](../../../compatibility/upstream/source-manifest.json).
Storage v2 protobuf SHA256 is
`a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.
V4 authentication keeps the [signed-download](signed-download.md) and
[signed-upload](signed-upload.md) contracts without a second signing format.

Fresh fetched HTML hashes, checked on the contract date:

| Source | SHA256 |
| --- | --- |
| PAP overview | `4d30750fd9ab9974ac30cdbef75405a525a28b8f40929d4f990af8d2573f3a36` |
| PAP management | `5506b058732760ee3a9b45a424b6c754c947588159942873f7a3d027daff1176` |
| Bucket resource | `9afc8a00ab38410dce9a3067d1804a2596b18a0aa36f5e620b0a34ba485d444d` |
| Organization constraints | `a920116fb5b042a86b38a01eff2d166c17c2350ee46e359460a261707e3a230c` |
| Bucket PATCH permissions | `0180ec381d347d8a5e1381154856945beaf97613e560debc8eb9d888ec8f45a6` |
| Bucket PUT permissions | `45746ebad0f3e26fa530aea94befe85fd9d94388978e69b4775e556cd95255bb` |

SPEC-PAP-EDGES records local choices: current JSON values exclude legacy
`unspecified`; null/invalid types return 400 `invalidArgument`; omitted PUT PAP
preserves the configured value; PAP policy rejection uses 412 `conditionNotMet`. Submitting
an unchanged public binding under PAP also fails. Existing IAM, XML and signing
gaps remain open; this slice does not establish provider message/error precedence.
The official [Go v1.70.0 enum reference](https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest)
deprecates `PublicAccessPreventionUnspecified` in favor of inherited. We retain
400 for the legacy JSON spelling because the current bucket reference enumerates
only inherited/enforced; older clients sending unspecified remain a known gap.

## Supported contract

`iamConfiguration.publicAccessPrevention` round-trips through insert, PATCH,
PUT, GET, list, projections and `fields` in both compatibility profiles. Response
metadata reports configured state, never replaces `inherited` with effective
parent enforcement. Empty/unknown IAM configuration children follow the existing
forward-compatible ignored-input rule. `uniformBucketLevelAccess` and legacy
`bucketPolicyOnly` accept `{"enabled":false}` without `lockedTime` as a no-op,
matching the modeled disabled state. Enabling them, empty/null configurations,
invalid enabled types and any lockedTime return 501. Responses still omit these
two fields; this fields-parity deviation is recorded in SPEC-PAP-EDGES. ACLs and
uniform-access transitions remain outside this slice.

PATCH and PUT preserve omitted PAP. Only an explicit value changes configured
PAP. PUT omission preservation is a local choice without real-GCS verification.
Every successful metadata mutation advances
metageneration once. Match/NotMatch conditions and both required mutation
permissions for actual PAP changes are checked in the same transaction. Failed
updates preserve state.
Schema version **13** adds a checked boolean with an inherited default; existing
catalogs migrate transactionally and reopen without changing objects or IAM.

Effective enforcement is the bucket flag OR the project's local parent fixture.
Strict authorization, overwrite-delete permission and public cache classification
read that state in the same transaction as the selected resource or mutation.
Existing public bindings and their IAM etag remain unchanged. Neither `allUsers`
nor `allAuthenticatedUsers` authorizes JSON/XML reads, listing or writes under
effective PAP. Registered identities' explicit project/user/service-account grants
still apply. An invalid token or signature never becomes an anonymous request.
The public URL helper constructs URLs without changing any permission.

Valid V4 RSA GET/HEAD/PUT uses the signer's effective nonpublic grants. Creator
signers can create new objects but require delete permission to overwrite. A
signer with only an overridden public grant is denied. Tampering preserves the
existing generation, metadata and bytes. Enforcement/revocation is rechecked at
publication after upload admission; no whole-object buffering is introduced.

Strict tests cover enforcement. Permissive mode remains an explicitly
unauthenticated development profile and is not evidence of protected access.

## Local parent policy fixture

Add the optional top-level field to the strict `identity_file`:

```json
{
  "public_access_prevention_projects": ["local-project"],
  "identities": [
    {
      "id": "local-admin@example.invalid",
      "tokens": ["local-admin-token"],
      "project_roles": {"local-project": ["roles/storage.admin"]}
    }
  ]
}
```

This is **LOCAL_SIMULATION** of an already-resolved parent organization/project
constraint. It supports at most 64 distinct valid project identifiers within the
existing 64 KiB registry limit. The registry rejects duplicates; direct catalog
callers' duplicate valid projects are idempotently deduplicated. The server loads
it before seeding/listening and reloads it only on restart. An absent/empty list
means no parent enforcement.
Removing a fixture on restart reactivates stored public bindings for inherited
buckets; an explicitly enforced bucket remains enforced. The fixture is held in
a connection-local temporary table, never serialized into bucket metadata.
Embedded users install it with `Catalog::configure_public_access_prevention`
before serving. No Google contact, organization hierarchy, policy mutation API,
propagation delay or external cache invalidation is simulated.

## Acceptance cases

| Test | Required behavior |
| --- | --- |
| `pap_metadata_insert_patch_update_fields_conditions_and_restart` | Both profiles, default/enforced insert, GET/list/fields, PATCH/PUT, omitted fields, atomic Match/NotMatch, revision and restart |
| `pap_invalid_and_unsupported_settings_preserve_resources` | Invalid/legacy values 400; unsupported UBLA/legacy enable, lock and malformed forms 501; failed insert never creates a bucket; failed mutations preserve metadata |
| `pap_disabled_uniform_access_noop_matches_python_client` | Exact Python 3.4.1 IAM configuration body works on insert/PATCH/PUT; legacy disabled alias is a no-op |
| `pap_suppresses_public_reads_writes_preserves_bindings_and_reactivates` | Both public principal sets, anonymous/registered JSON and XML GET/HEAD/PUT/upload/list/delete denial, bearer success, policy preservation, 412 public submission, disable/reactivation and explicit bucket grant |
| `pap_signed_get_head_put_use_signer_grants_and_reject_tampering` | Valid signed GET/HEAD/PUT under PAP, creator/overwrite requirements, public-only signer denial, explicit bucket signer success and invalid-signature preservation |
| `pap_parent_fixture_inheritance_restart_and_release` | Existing/new buckets, inherited metadata versus enforcement, unaffected project, cannot bypass parent with inherited, signed GET/PUT, restart/release and explicit enforcement persistence |
| `pap_migrates_version_twelve_and_persists_configured_state` | Version 12 upgrade to 13, inherited default, database constraint, persisted enforcement; existing older-schema migration corpus remains required |
| `pap_is_rechecked_after_upload_admission_and_before_reads` | Bucket/parent enforcement after authorization denies all shared read/mutation paths and an admitted upload, preserving object and stored policy and removing staging files |
| `pap_resumable_finalization_rechecks_public_initiator` | Both public principal sets can initiate before enforcement but cannot publish afterward, even through the session capability; existing object metadata/generation/bytes survive |
| `pap_copy_rewrite_compose_recheck_public_source_and_destination_grants` | Public-only copy/rewrite/compose denied under source or destination PAP, including a pre-admitted multi-call rewrite; source/destination metadata/generation/bytes survive and compose deletes no source |
| `pap_signed_requests_suppress_all_authenticated_users_grants` | Valid registered public-only signer GET/HEAD/PUT denied under PAP without changing object metadata/generation/bytes |
| `pap_parent_fixture_rejects_malformed_and_unbounded_projects` | Reject malformed, duplicate, invalid and over-limit fixture projects |
| `pap_mutations_require_update_and_set_iam_permissions` | Update-only authority can change labels while repeating or omitting either stored PAP value; both mutation methods reject actual PAP changes atomically |
| `TestPublicAccessPrevention` | Pinned Go BucketAttrs insert/get/update, anonymous denial, policy preservation/rejection, unmodified official signed GET/PUT at a guarded default-port authority, disable/reactivation |

## Commands for the runnable signed journey

Copy this block into `signed_journey` after its private GET/PUT checks. It uses
the guide's existing `signed_endpoint`, `signed_work`, `signed_sign` and
`signed_curl`, and a new object name so its create-only PUT is repeatable once
per isolated journey. Signing always uses the actual final endpoint, including
host and port. HTTPS continues to use the guide's exported CA.

```sh
signed_curl --fail -X PATCH -H 'Authorization: Bearer guide-admin-token' \
  -H 'Content-Type: application/json' \
  --data '{"iamConfiguration":{"publicAccessPrevention":"enforced"}}' \
  "$signed_endpoint/storage/v1/b/signed-guide-bucket" > "$signed_work/pap.json"
jq -e '.iamConfiguration.publicAccessPrevention == "enforced"' "$signed_work/pap.json" > /dev/null
test "$(signed_curl -o /dev/null -w '%{http_code}' \
  "$signed_endpoint/signed-guide-bucket/private.txt")" = 401
pap_get=$(signed_sign GET "$signed_endpoint" private.txt)
signed_curl --fail "$pap_get" > "$signed_work/pap-download.txt"
cmp "$signed_work/input.txt" "$signed_work/pap-download.txt"
pap_put=$(signed_sign PUT "$signed_endpoint" pap-uploaded.txt \
  --header 'content-type:text/plain' --header 'x-goog-if-generation-match:0')
signed_curl --fail -X PUT -H 'Content-Type: text/plain' \
  -H 'x-goog-if-generation-match: 0' --data-binary @"$signed_work/input.txt" "$pap_put"
pap_uploaded_get=$(signed_sign GET "$signed_endpoint" pap-uploaded.txt)
signed_curl --fail "$pap_uploaded_get" > "$signed_work/pap-uploaded.txt"
cmp "$signed_work/input.txt" "$signed_work/pap-uploaded.txt"
test "$(signed_curl -o /dev/null -w '%{http_code}' "${pap_get}00")" = 403
echo 'PASS: enforced PAP denies anonymous access and permits valid signed GET/PUT'
```

Expected PATCH status is 200 with configured PAP `enforced`; anonymous GET is
401; signed GET returns exact input bytes; signed PUT is empty 200; changed
signature is 403 `SignatureDoesNotMatch`. The final line above is the only
explicit success output. Keep PAP enforced across restart and freshly sign the
new endpoint. These commands are now included in the runnable guide. The final
native/source-image and strict-Compose execution, public-policy preservation,
disable/reactivation and virtual-host interactions are recorded in the
[combined evidence](../evidence/signed-urls.md). The original feature evidence
remains wire/Go-only; final container and guide claims belong to that integrated set.
