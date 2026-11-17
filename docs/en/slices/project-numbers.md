# Project numbers

Scope: BEHAVIOR-COMMON-014-PROJECT-NUMBER, with the project number parts of
JSON-BUCKETS-002/005/006, JSON-PROJECTS-SERVICEACCOUNT-001, BEHAVIOR-BUCKET-001
and BEHAVIOR-BUCKET-004. Cloud Storage renders a bucket's project by number and
takes either the project ID or the number as input (recorded 2026-11-17, case
`prj-number-inputs`; see the
[eighth recording](../evidence/provider-differential-gap-probes.md#eighth-recording-project-numbers-as-input)).
gcs-mimic keeps no project registry, so a project's number is a pure function
of its ID.

## Acceptance contract

- NUMBER-DERIVED: a project ID starts with a letter, so decimal digits that fit
  an unsigned 64-bit number are that project number, leading zeros included
  (`00042` is 42). Any other project ID, including a decimal that overflows,
  gets
  `100000000000 + (SHA-256(ID) mod 900000000000)`, the digest read as one
  big-endian unsigned integer: a twelve-digit number that is the same across
  restarts, data directories and machines. floci-gcp derives the same number,
  so a project ID used against both emulators renders alike. Project `local`
  is `902648055030`.
- NUMBER-RENDERED: the number appears as JSON `projectNumber` (native profile),
  Storage v2 `Bucket.project` (`projects/{number}`), the project-team entities
  of the `projectPrivate` ACL and of predefined ACLs (`project-owners-{number}`),
  the IAM-to-ACL mapping of `projectOwner:ID` style members, the service agent
  `service-{number}@gs-project-accounts.iam.gserviceaccount.com`, and the
  `Project {number} may not create storageClass ...` refusal. The
  fake-gcs-server profile keeps reporting `projectNumber` `"0"`, as that server
  does; its ACL entities still carry the derived number.
- NUMBER-INPUT: a request names its project by ID or by number in the JSON
  `project` parameter (bucket insert and list, soft-deleted list), the JSON
  project path (`projects/{project}/serviceAccount` and `hmacKeys`), the XML
  `x-goog-project-id` header (bucket list and creation), Storage v2
  `CreateBucket` and `ListBuckets`, and the legacy v1 `project` field. A number
  turns back into the ID before authorization and before the catalog, so strict
  mode checks the grants of that ID. The ID comes from the projects the identity
  file names (role grants and public access prevention projects), then from the
  projects that own a bucket, a soft-deleted bucket or an HMAC key, the first in
  byte order whose number matches.
- NUMBER-UNKNOWN: a number no known project derives from names a project of its
  own, stored under the digits, whose number is that same number.
- NUMBER-ZERO: Storage v2 `projects/0`, in any spelling of zero, is
  INVALID_ARGUMENT `Project id: 0 is invalid or not found` with a
  `google.rpc.BadRequest` repeating it, as recorded for `projects/0`.
- NUMBER-TOKENS: bucket, soft-deleted bucket and HMAC key page tokens bind the
  project's number instead of its spelling, so a listing continues whether the
  next request names the project by ID or by number. A token issued before
  tokens bound the number still opens for the spelling it bound.
- NUMBER-MIGRATION: schema version 55 renames the project-team entities stored
  with number `0` in the bucket ACL, default object ACL, object ACL, soft-deleted
  bucket ACL and retained object ACL of a bucket whose project is not a
  number, in one transaction with the version bump. A numeric
  project's `-0` team names project 0 and stays. A row that does not decode is
  left for its reader to report. Stored object owners and resumable sessions
  keep the project ID or the predefined ACL name and need no change; upload
  replay records expire on their own.

## Differential normalization

The harness used to replace the emulator's `0` in team entities, the service
agent address and the class-location refusal. Those three rules are gone: the
emulator's number appears in the bucket creation responses, so the normalizer
hides it with the `<project-number>` placeholder exactly as it hides the number
Cloud Storage reports.

## Inferred and not modeled

- Inferred from the recorded JSON and Storage v2 routes: the XML header, the
  legacy v1 fields, HMAC key creation and listing and soft-deleted listings take
  a number the same way, and a page token continues across the two spellings.
- Not modeled: Google-assigned numbers, and Cloud Storage's refusal of a number
  or ID that names no project. Only `projects/0` gets the recorded refusal.

## Tests

`an_id_derives_a_stable_twelve_digit_number`,
`decimal_digits_are_their_own_number`,
`a_project_number_names_the_project_its_resources_render`,
`a_number_no_known_project_has_names_a_project_of_its_own`,
`strict_mode_reaches_a_granted_project_by_number_before_it_owns_anything`,
`grpc_lists_a_project_by_the_number_its_buckets_render`,
`project_name_takes_back_rendered_numbers_and_rejects_zero`,
`page_tokens_keep_their_bytes_and_open`,
`hmac_key_list_tokens_keep_their_bytes_and_open`,
`only_the_owning_projects_zero_numbered_teams_are_renamed`,
`stored_text_changes_only_when_an_entity_is_renamed`,
`opening_an_older_catalog_renames_zero_numbered_teams_to_the_owning_project`,
`a_failed_rename_leaves_every_row_and_the_version_unchanged`, and the
differential case `prj-number-inputs`.
