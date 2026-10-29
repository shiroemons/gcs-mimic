# Project HMAC keys and service agent

Scope: JSON-PROJECTS-HMACKEYS-001..005 (`projects.hmacKeys`
create/delete/get/list/update) and JSON-PROJECTS-SERVICEACCOUNT-001
(`projects.serviceAccount.get`). All six rows remain PARTIAL: the scoped
acceptance cases below pass, but provider error details, service account
existence, requester billing and HMAC signature use are outside this slice.

## Acceptance contract

- HMAC-CREATE: `POST /storage/v1/projects/{projectId}/hmacKeys` requires
  `serviceAccountEmail`, takes no body (or the identical form parameters) and returns `storage#hmacKey` with the
  secret and the metadata. The access ID is 61 characters (`GOOG1E` plus 55
  base32 characters) and the secret is a 40-character base64 string. A new key
  is ACTIVE; `id` is `{projectId}/{accessId}`; `selfLink` uses the advertised
  API base URL; `timeCreated` equals `updated`.
- HMAC-SECRET-ONCE: get, list and update return metadata only; the secret is
  never returned again.
- HMAC-GET: get returns `storage#hmacKeyMetadata` for a key in the path project,
  including DELETED keys. An unknown access ID or a key owned by another project
  returns 404 notFound.
- HMAC-LIST: list returns `storage#hmacKeysMetadata` ordered by access ID.
  `serviceAccountEmail` filters exactly; `showDeletedKeys=true` includes DELETED
  keys, which are hidden by default. `maxResults` is a uint32; 0 or omission uses
  1000 and larger values are capped at 1000. `nextPageToken` is present only when
  more keys follow; tokens are bound to the project and both filters, and a token
  replayed with different filters or corrupted returns 400 invalidParameter.
- HMAC-UPDATE: PUT accepts an `HmacKeyMetadata` body. Only `state` (ACTIVE or
  INACTIVE) is writable and it is required. A supplied `etag` must match the
  current etag or the request returns 412 conditionNotMet without mutation.
  Output-only and unrecognized fields are ignored, so read-modify-write clients
  can send the whole resource back. `state: DELETED` returns 400 invalidArgument
  directing callers to DELETE. Every successful update advances the etag and
  `updated`.
- HMAC-DELETE: DELETE of an INACTIVE key returns 204, marks it DELETED, advances
  the etag and erases the stored secret in the same transaction. Deleting an
  ACTIVE key returns 400 badRequest; deleting or updating a DELETED key returns
  400 badRequest. Rejected requests leave the key unchanged.
- HMAC-LIMIT: at most ten keys that are not DELETED exist per service account
  email. The eleventh creation returns 400 badRequest; deleting a key frees a
  slot. The count and insert run in one immediate transaction, so concurrent
  creations cannot exceed the limit.
- HMAC-CONCURRENCY: concurrent conditional updates that carry the same etag
  admit exactly one writer; the others receive 412.
- HMAC-PERSIST: catalog schema 32 adds the `hmac_keys` table; keys, states and
  etags survive restart, and an older catalog migrates without losing data.
- HMAC-AUTH: strict mode requires `storage.hmacKeys.create`, `.get`, `.list`,
  `.update` and `.delete` in the path project, from the pinned predefined-role
  snapshot. `roles/storage.hmacKeyAdmin` grants all five, `roles/storage.viewer`
  grants get/list, and `roles/storage.admin` grants none. Anonymous callers get
  401 required and unknown tokens 401 authError before validation.
- SA-GET: `GET /storage/v1/projects/{projectId}/serviceAccount` returns
  `{"kind": "storage#serviceAccount", "email_address": ...}` with the Google form
  `service-PROJECT_NUMBER@gs-project-accounts.iam.gserviceaccount.com`. A numeric
  project identifier is its own number; a named local project reports 0, the same
  `projectNumber` the bucket resource reports, so the address is deterministic.
- SA-AUTH: the method requires `resourcemanager.projects.get`, which the
  storage-only role snapshot does not carry. Strict mode accepts any local role
  grant in the path project as project visibility, rejects other projects with
  403 and anonymous callers with 401.
- COMMON: `fields` is validated against the pinned Discovery schemas (`HmacKey`,
  `HmacKeyMetadata`, `HmacKeysMetadata`, `ServiceAccount`); `userProject` and
  other unsupported system parameters return 400 badRequest; unknown query
  parameters return 400 invalidParameter; non-empty bodies on bodiless methods
  return 400 invalidArgument; other HTTP methods return 405 with `Allow`.
- SDK: the pinned Go SDK creates, gets, lists with both filters, rejects ACTIVE
  deletion and stale etags, deactivates, deletes and reads service agents; the
  pinned Ruby SDK does the same through `create_hmac_key`, `hmac_keys`,
  `inactive!`, `delete!` and `service_account_email`, and its strict run
  confirms that `roles/storage.admin` cannot create keys.

## Sources and local choices

Checked 2026-10-29 (pages "Last updated 2026-10-26 UTC"): the
[HMAC key resource](https://docs.cloud.google.com/storage/docs/json_api/v1/projects/hmacKeys)
and its create, delete, get, list and update method pages, the
[service account resource](https://docs.cloud.google.com/storage/docs/json_api/v1/projects/serviceAccount),
the [HMAC keys guide](https://docs.cloud.google.com/storage/docs/authentication/hmackeys)
and [IAM permissions for JSON methods](https://docs.cloud.google.com/storage/docs/access-control/iam-json).
Field names and kinds follow the pinned Discovery schemas in
`compatibility/upstream/json-schemas.json`.

The documents establish the fields, the secret-only-on-create rule, the
ACTIVE/INACTIVE update rule, `showDeletedKeys`, the 61-character service-account
access ID, the 40-character secret, the ten-key limit and the permissions. They
do not state the status codes for deleting an ACTIVE or DELETED key, updating a
DELETED key or exceeding the limit; gcs-mimic returns 400 badRequest with an
actionable message for each. They also do not document a default page size or
the etag format: gcs-mimic uses 1000 and an opaque per-key revision.

Local choices:

- No IAM directory exists, so any syntactically valid email (one `@`, non-empty
  parts, at most 254 bytes, no whitespace) names a service account; Google would
  reject accounts that do not exist.
- Keys are scoped to the path project identifier as given. A project number and
  a project ID are different local projects.
- The secret is stored in the catalog so that a later HMAC signature verifier can
  use it; deletion erases it. The catalog is local test state, not a secret store.
- Requester billing (`userProject`) is rejected rather than ignored.
- The pinned Ruby generated client repeats `serviceAccountEmail` as a
  form-encoded body on create. As for `lockRetentionPolicy`, only an empty body
  or form parameters identical to the URL query are accepted; any other body
  returns 400 invalidArgument before mutation.

## Not in scope

XML API HMAC management (EXT-XML-028..031), HMAC-signed requests and signed URLs
(DESIGN-031), the 60-second activation delay, `restrictAuthTypes`, user-account
HMAC keys and provider verification of the local error choices.

## Verification

Results are recorded in `compatibility/features.json` for the six rows.

- `cargo test --locked --test project_resources_http`: lifecycle, state rules,
  etags, restart, listing filters, paging, limit, service agent and strict
  authorization.
- `cargo test --locked --lib hmac_keys` and `cargo test --locked --lib json::projects`:
  generated shapes, schema migration, concurrent limit and etag races, routing,
  query and body validation, page-token binding and service agent format.
- `cargo test --locked --test unsupported_http` and `--lib server::unsupported`:
  the six methods leave the unsupported-route inventory; other routes still fail
  fast.
- `just check-sdk`: `TestHMACKeysAndServiceAccount` (Go) and
  `ruby-project-service-account`, `ruby-project-hmac-keys`,
  `ruby-project-hmac-keys-strict-denied` (Ruby, permissive and strict runs).
