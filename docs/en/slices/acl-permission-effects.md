# ACL permission effects

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-017/018/019/020/021 and the bucket, default object and object ACL
operations. The feature ledger owns implementation status. This evidence does
not narrow those rows to object reads: bucket ACL effects, immutable uploader
ownership, trusted group/domain/project membership and canned ACL publication
remain required acceptance cases.

## Contract

The [ACL guide](https://docs.cloud.google.com/storage/docs/access-control/lists)
(updated 2026-10-30), [IAM relationship](https://docs.cloud.google.com/storage/docs/access-control/iam#relation_to_acls),
[uniform bucket-level access](https://docs.cloud.google.com/storage/docs/uniform-bucket-level-access)
and [public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention)
define the permission contract. IAM and ACL grants are additive. PAP suppresses
public grants; it does not suppress an explicit principal's IAM or ACL grant.
UBLA disables object ACL grants and ACL management operations; synchronized
legacy bucket IAM permissions remain effective. An object ACL applies to its selected
generation, not to other generations or the containing bucket.

The committed JSON Discovery baseline is revision `20261013`, source SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`
in `compatibility/upstream/json-schemas.json`. Storage protobufs are pinned to
googleapis revision `d73dfb3c3be52245625b951b9c0bcf65163c549f`.
fake-gcs-server v1.56.1, revision
`896ece51e49eed0946089f3089f6343855c0d106`, is a comparison implementation.

## Object read slice

The catalog defers an IAM-denied object read until it selects the generation,
then evaluates that generation's READER/OWNER entries in the same transaction
as existence and conditions. Matching registered user identities,
`allAuthenticatedUsers` and `allUsers` grant reads. A missing or inaccessible
generation remains denied before conditions disclose its metadata. IAM reads
retain their existing existence and precondition behavior.

JSON metadata/media, XML GET/HEAD and gRPC GetObject/ReadObject use the same
selection. ACL reads do not grant bucket listing, another object's reads,
metadata changes, deletes or ACL projection. Customer encryption-key checks
still apply. Anonymous cache policy includes the selected generation's public
ACL and current PAP/UBLA state.

Copy, compose, move and rewrite recheck the pinned source in their publication
transaction. An implicit-generation rewrite still rejects a replaced source
with its existing 412; an explicit-generation rewrite can continue on its
original generation. Copy retry completions store the actual source generation
alongside their existing metadata. Replays recheck that generation's current
ACL/PAP/UBLA instead of the new live generation. Older completions without a
source proof require bucket IAM read authority. This changes no blob write,
rename or filesystem synchronization step.

Every new BidiReadObject RPC reauthorizes its handle's original generation.
The handle table mutex is released before the short catalog transaction. IAM
readers retain the existing opened snapshot after deletion; another principal
cannot reuse that handle. An already admitted stream keeps its bounded opened
snapshot; cancellation does not retract bytes already read.

### Cases and verification

The real local server cases in `tests/acl_permission_effects/` are:

- `object_acl_reads_are_generation_scoped_on_disk_and_memory`: metadata/media,
  XML GET/HEAD, conditions, invalid credentials, private replacement, old
  generation and revocation; bucket/list/other object and mutation negatives.
- `public_acl_pap_uniform_access_and_restart_recheck_the_selected_generation`:
  authenticated versus anonymous access, public caching, explicit OWNER read,
  PAP, UBLA and disk restart.
- `object_acl_read_does_not_bypass_customer_key_validation`: required and
  matching customer keys, exact bytes, and ACL denial.
- `grpc_object_acl_read_checks_selected_generation_and_revocation`: selected
  generations, invalid and other credentials, private replacement and revocation.
- `source_acl_copy_replay_keeps_original_generation_and_rechecks_current_controls`:
  fixed retry result, PAP/UBLA, changed fingerprint, disk restart and revocation.
- `source_acl_authorizes_copy_compose_move_and_fixed_generation_rewrite`:
  destination writer without source IAM, copy/compose/move, explicit and
  implicit rewrite generations, replay and revocation.
- `legacy_copy_completion_requires_iam_when_original_source_proof_is_absent`:
  legacy completion denial for ACL-only readers and successful IAM replay.
- `bidi_read_handle_reauthorizes_fixed_generation_for_each_new_rpc`:
  disk/RAM handle reuse, different principal, exact old bytes, revocation,
  public grants, PAP, UBLA and IAM authority.
- `bidi_read_iam_handle_keeps_its_opened_snapshot_after_generation_deletion`:
  IAM snapshot compatibility and different-principal denial.

Catalog coverage additionally includes
`object_acl_revocation_rechecks_admitted_reads_and_keeps_opened_snapshot`
(disk/RAM, concurrent revocation and stale conditions). Copy coverage includes
`copy_rechecks_source_object_acl_after_streaming_before_publication`
(stream checkpoint, revoked source and absent destination).

Recorded completed checks before the BidiRead addition: seven ACL wire cases,
fourteen existing copy cases and twenty-six existing rewrite cases passed.
The earlier integrated checkpoint also passed 122 authentication, nineteen
compose and seven move cases. The catalog concurrent-revocation case passed.
The final focused module runs passed three read cases, three derived cases and
two BidiRead cases. The gRPC selected-generation case also passed. The copy
stream-checkpoint unit passed. Compilation and production lint passed after
extracting the handle-resume helper. Target clippy for the library, binary and
ACL wire target passed with warnings denied. The strict authentication unit
module also passed thirteen cases, including tokenless-principal registration
without bearer/JWT authentication.

The subsequent bucket-role, IAM-mirror and legacy migration cases now also pass.
The whole ACL wire target passes sixteen cases; this does not close its wider
feature rows. Three startup reconciliation unit cases, two mirror transaction
unit cases and two typed startup-failure cases also pass. Target compilation,
clippy with warnings denied and production lint pass; production lint reports
zero violations and one existing reasoned stringified-error suppression.

Commands for the focused gates:

```sh
cargo check --locked --offline
cargo test --locked --offline --test acl_permission_effects -- --nocapture
cargo test --locked --offline --test acl_permission_effects ::read:: -- --nocapture
cargo test --locked --offline --test acl_permission_effects ::bidi:: -- --nocapture
cargo test --locked --offline --test acl_permission_effects ::derived:: -- --nocapture
cargo test --locked --offline --lib object_acl_revocation_rechecks_admitted_reads_and_keeps_opened_snapshot
cargo test --locked --offline --lib copy_rechecks_source_object_acl_after_streaming_before_publication
cargo test --locked --offline --lib auth::tests:: -- --nocapture
cargo test --locked --offline --lib auth::acl_identity::tests:: -- --nocapture
cargo test --locked --offline --lib storage::catalog::principals::tests:: -- --nocapture
cargo test --locked --offline --lib storage::catalog::acl::bucket_policy::tests:: -- --nocapture
cargo test --locked --offline --bin gcs-mimic authentication_setup_failure -- --nocapture
cargo test --locked --offline --test acl_http --test auth_http -- --nocapture
cargo test --locked --offline --test auth_http --test object_copy_http --test object_compose_http --test object_move_http --test rewrite_http
cargo xtask lint
cargo clippy --locked --offline --lib --bin gcs-mimic --test acl_permission_effects -- -D warnings
```

The final focused runs used `CARGO_INCREMENTAL=0` after cleanup of the shared
build cache. Existing ACL wire coverage passed eleven cases and strict
authentication wire coverage passed 122 cases after the mirror change.

## Bucket roles, IAM views and trusted user aliases

Bucket READER grants object listing and bucket metadata without ACL projection.
WRITER adds create, replacement and deletion, without reading an existing
object's bytes or updating its metadata. OWNER adds bucket metadata and ACL
management. The matcher uses the pinned legacy bucket IAM permission data;
these grants do not extend to another bucket, project bucket listing, bucket
deletion or object data reads. The existing object protections and atomic
overwrite-delete checks remain in the publication transaction.

Bucket ACL writes replace the representable legacy bucket IAM grants in the
same transaction; unrelated role bindings remain unchanged. IAM writes update
the corresponding bucket ACL, with concentric roles collapsed to their
strongest grant. Policy etags still reject stale changes. ACL management is
disabled by UBLA, while synchronized legacy bucket IAM permissions survive.
PAP suppresses public grants in either view without suppressing explicit users.

ACL changes apply an entity/role delta to IAM bindings. Unchanged grants retain
their original IAM principal IDs, including when an explicit canonical alias
changes. An unchanged full ACL echo does not rewrite stable IDs or policy etags.

Schema 43 records whether each bucket's ACL and IAM views have been bound.
Startup reconciles an older unbound bucket's explicit stored ACL with its IAM
permissions in one short transaction, preserving their permission union and
original IAM bindings. A bucket without stored ACL entries does not gain virtual
default grants. New and recreated buckets start bound. Subsequent restarts use
IAM as the authoritative source for that bound mirror, so an old alias cannot
be promoted into a new independent IAM grant after alias reuse. The marker and
view changes commit together; failed transactions leave the old marker and
views unchanged. This migration introduces no filesystem durability step.

Startup installs the registry and completes reconciliation before admission.
Failure closes the catalog and retains the primary catalog error and a separate
optional cleanup error as typed causes, with catalog diagnostics preserved.

When an IAM change alters the bucket ACL's actual entries, the local catalog
advances the bucket ACL's containing metageneration. A non-legacy IAM change
that leaves its ACL unchanged preserves all bucket metadata. The mirror itself
is required by the official IAM relationship; the cross-API metageneration
choice is a local consistency rule, not verified provider behavior.

The trusted registry accepts an optional canonical `acl_entity` user alias.
Aliases cannot collide with another principal's alias or natural user entity,
cannot represent groups/domains/project teams and are bounded within the
existing registry limit. A principal's original local ID remains unchanged.
IAM and ACL user matching use the same trusted identity. Startup installs a
bounded immutable principal map before request or seed/sync admission, without
copying bearer tokens or key material into the catalog. Reconfiguration is
rejected. This map currently supports canonical mirror lookup; persisted upload
identity rebinding and its initial capability ceiling remain separate work.

Additional passing real-server cases are:

- `bucket_acl_roles_grant_bucket_and_write_actions_without_object_reads`:
  disk/RAM READER/WRITER effects, replacement and deletion, and object-read,
  metadata, ACL and project/bucket isolation negatives.
- `bucket_acl_owner_changes_metadata_and_acls_without_other_resource_authority`:
  bucket metadata/default ACL/ACL updates and absence of object-read, object
  metadata, another bucket and bucket-delete authority.
- `bucket_acl_public_grants_obey_pap_ubla_and_restart_without_suppressing_iam`:
  authenticated/anonymous grants, PAP, restart, UBLA's legacy bucket authority
  and continued ACL-operation rejection.
- `bucket_acl_and_legacy_iam_share_revocation_without_changing_other_bindings`:
  disk/RAM IAM-to-ACL, ACL-to-IAM, role change, stale etag, revocation, unchanged
  unrelated bindings/metadata and exact private object bytes.
- `trusted_user_alias_matches_iam_and_acl_without_changing_the_local_principal_id`:
  explicit alias lookup, IAM mirror/revocation, restart with a changed alias,
  other-principal denial and rejection of the local ID as a bearer credential.
- `unrelated_acl_edits_preserve_stable_iam_ids_across_alias_changes`:
  unrelated edits and full ACL echoes preserve stable IAM IDs; reuse of an old
  alias by another principal grants neither bucket authority nor the original
  principal's object ACL authority across two restarts.
- `legacy_bucket_views_reconcile_before_ready_and_keep_role_union_and_restart_etags`:
  schema 42 upgrade, independently stored legacy views, role union, PAP/UBLA,
  revocation, exact bytes and unchanged etags on a second restart.

Catalog cases cover partial startup reconciliation followed by changed aliases,
new/deleted/recreated buckets, absent legacy ACL rows, oversized or malformed
legacy entries, and rollback after a staged ACL change when the IAM update
fails. Startup cases retain primary and cleanup error causes and verify that
the closed directory can be reopened after failure.

Two alias unit cases also pass: natural mapping and explicit aliases preserve
the original ID, IAM/ACL matchers agree, malformed aliases and both orderings of
cross-principal collisions are rejected, and a forged upload-snapshot alias is
not accepted. Group/domain/project membership is not inferred from an alias.

## Immutable owner persistence checkpoint

The [objects resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects)
and ACL guide distinguish the original uploader from additional ACL OWNER
grants. Omitted owner grants are added automatically and lower grants are
promoted to OWNER. Object replacement assigns the new uploader as owner. The
guide describes anonymous uploads as owned by the bucket project's owners.

Schema 44 stores a nullable typed owner independently of ACL entries. New
single-request, derived and finalized resumable publications record the actual
registered principal ID; anonymous
and local administrative publications record the actual bucket project ID.
Older generations retain an unknown owner (`NULL`); migration does not guess an
uploader or add authority. An imported historical identity without proven owner
data likewise remains unknown at this checkpoint.

The owner grant is synthesized for ACL views and never stored as an independent
alias grant. ACL omission, entry deletion and role downgrade preserve OWNER in
the view. Read authorization uses the saved principal ID, so reusing an old
local canonical alias cannot transfer ownership. During strict startup, each
stored principal owner's display entity is resolved against the current
registry by that saved ID, one generation per short transaction before
admission. Missing or aliasless IDs have no invented display entity. Only a
changed owner value is updated; generation, metageneration, timestamps and bytes
stay unchanged. Permissive and trusted library profiles retain their recorded
display entity. New publications normalize the display entity against the
configured profile, including when their identity came from persisted upload
options.

After alias reassignment, a new explicit ACL grant to the old alias is a separate
grant: it takes effect and can be revoked. A full resource echo repeats the
current owner display. Changing the owner entity, supplying an invented
`entityId` or an invalid owner value is rejected under the existing local
output-only field policy. The exact provider behavior for changed output-only
fields has not been established. Arbitrary local IDs without a canonical alias
still upload and retain ownership by ID, while their Google-shaped owner field
is omitted. UBLA hides the owner and disables owner ACL authority; PAP does not
suppress the explicit uploader's authority.

Passing real-server cases are:

- `immutable_upload_owner_survives_acl_edits_overwrite_and_restart`: disk/RAM,
  selected-generation owner, owner preservation after omitted/deleted/downgraded
  ACLs, additional OWNER grants, replacement uploader, wrong owner inputs,
  bucket/delete isolation, PAP, UBLA and disk restart.
- `actual_owner_id_cannot_be_replaced_by_reusing_its_canonical_alias`: full
  projection echoes, downgrade and deletion, changed/reused aliases after
  restart, original-ID read authority, new explicit READER/OWNER grants to the
  reused alias, revocation and absence of synthesized owner entries in storage.
- `owner_upgrade_does_not_guess_legacy_uploaders_and_aliasless_uploads_keep_actual_ids`:
  schema 43 upgrade, unknown historical owner, aliasless uploads, saved actual
  identity, revoked bucket IAM, other-principal denial and restart.

Three owner unit cases pass: migration rollback/retry and unknown uploaders;
strict lookup of current, missing and aliasless IDs with profile boundaries; and
partial reconciliation followed by a changed registry, preserving IDs, bytes and
metadata and avoiding writes on a repeated unchanged pass.

```sh
CARGO_INCREMENTAL=0 cargo check --locked --offline --lib --bin gcs-mimic --tests
CARGO_INCREMENTAL=0 cargo test --locked --offline --test acl_permission_effects ::owner:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::catalog::object_owner::tests:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::catalog::admin::publication::tests:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::resumable_http_tests::fault_injection::upload_ -- --nocapture
```

After extracting record construction and validation helpers, the complete ACL
wire target passed nineteen cases, administrative publication coverage passed
seventeen cases, and the existing upload crash and PowerLoss cases both passed.
The publication cases intentionally panic two file jobs to verify supervised
worker recovery. Compilation, target clippy with warnings denied and production
lint passed, with zero production violations and one existing reasoned
stringified-error suppression.

This checkpoint adds no blob write, rename or filesystem synchronization step.
The broader publication, restore/move, import/export, appendable and protocol
acceptance cases remain required below; their ownership semantics are not
complete merely because these six cases pass.

## Object OWNER authority and replay checkpoint

The catalog now defers exactly object get, update, getIamPolicy and
setIamPolicy admission when bucket authority is insufficient. The selected
generation and its immutable uploader or ACL OWNER grant are checked in the
same transaction as its metadata or ACL operation. READER continues to grant
only object reads. Object OWNER does not grant bucket listing, object creation,
deletion, bucket ACL changes or the separate retention permissions.

Full object projections evaluate getIamPolicy separately for each listed
generation in the projection transaction. An admission context is not itself a
grant. Bucket testIamPermissions and the bucket permission probe continue to
report actual bucket authority. A read context cannot be passed to the object
ACL projection API to reveal ACLs or owners.

Passing real-server cases currently include:

- `object_owner_grants_metadata_and_acl_authority_only_on_its_selected_generation`:
  disk/RAM metadata and ACL read/write, full echoes, separate additional OWNER
  grant and revocation, generation isolation, stale conditions, retention and
  bucket/create/delete negatives, and the actual bucket permission probe.
- `object_acl_full_projection_rechecks_each_listed_generation_separately`:
  an authorized historical generation's ACL/owner is visible while other
  generations and objects remain hidden; revocation hides those fields too.
- `uploader_owner_authority_survives_bucket_revocation_and_obeys_uniform_access`:
  original uploader metadata/ACL authority after bucket grant revocation,
  full echo, PAP, restart, UBLA denial and recovery after disabling UBLA.
- `object_acl_limit_counts_implicit_owner_and_rolls_back_oversized_publications`:
  99 explicit grants plus the implicit owner fill the 100-grant limit while
  storage contains 99 entries; oversized publication, replacement and insertion
  fail without changing bytes, metadata or the generation allocator. Owner
  deletion/downgrade retains the grant, and an explicit echoed owner consumes
  its existing slot.
- `public_owner_roles_grant_metadata_and_acl_access_without_transferring_actual_ownership`:
  anonymous versus registered OWNER scopes, invalid credentials, metadata/ACL
  authority, absent object deletion authority, PAP, explicit user authority and
  revocation without changing the actual uploader.
- `metadata_mutation_rechecks_owner_revocation_after_request_body_admission`:
  disk/RAM `100 Continue` confirms body admission before revocation; completing
  the metadata body then returns 403 before its stale condition can disclose a
  revision, with exact metadata and bytes preserved.

Existing ACL CRUD coverage passed eleven cases, object metadata coverage passed
six cases and bucket IAM coverage passed twenty cases. Older expectations of an
empty object ACL or a resource without its uploader grant were corrected to
the guide's owner-preservation contract. Comparisons use sorted vectors without
deduplication, retaining exact roles, entry counts and surplus/duplicate
detection. Strict credential, authorization, precondition, generation isolation,
mutation rollback and ACL limit negatives remain exercised.

The upload/copy replay snapshot is a trusted typed value bound to the original
bucket, object name, generation and existing bucket incarnation. Its permission
check runs in the current catalog transaction, with current UBLA and project
checks. IAM, a current selected-generation OWNER grant, or the snapshot's
immutable uploader ID may authorize its ACL view. Historical explicit ACL
entries never prove a revoked delegate's authority. A deleted destination
generation therefore retains the original uploader's retry response without
granting another principal access. The stored completion JSON shape is
unchanged; the record's typed owner is re-resolved against the current registry
for each replay. Implicit OWNER is added only to the rendered view.

Three additional real-server cases passed:

- `full_upload_replay_keeps_original_owner_proof_after_generation_deletion`:
  disk/RAM fixed original ACL and owner, private replacement, deleted original
  generation, changed body/principal rejection, PAP and UBLA visibility.
- `upload_replay_resolves_saved_owner_id_against_current_aliases_without_rebinding_old_alias`:
  restart with changed/reused aliases, unchanged original generation and
  metageneration, current canonical owner, no old implicit alias in saved ACL
  entries, other-principal denial and deleted-generation replay.
- `full_copy_replay_binds_destination_owner_and_current_source_authority`:
  destination ownership, deleted-generation replay, wrong target rejection,
  source revocation/regrant, noAcl and selected owner fields.

```sh
CARGO_INCREMENTAL=0 cargo test --locked --offline --test acl_permission_effects ::owner_authority:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --test acl_permission_effects ::owner_replay:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --test acl_http -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --test object_metadata_http -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --test auth_http iam:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --test acl_permission_effects -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::catalog::acl::snapshot_tests:: -- --nocapture
```

The aliasless-owner logical-limit boundary also passes: its saved ID still
consumes one logical grant, so 99 rendered grants plus that owner fill the
limit; 100 explicit grants are rejected without replacing the old generation.
The expanded wire target passes all twenty-eight cases. The catalog case
`replay_snapshot_rechecks_current_delegate_grants_and_binds_immutable_owner_to_incarnation`
passes on disk/RAM, covering revoked delegate grants, wrong permission context,
resource matching, deleted-generation uploader proof, wrong incarnation and a
recreated bucket in another project. Target clippy passes with warnings denied.
The coordinated production lint gate passed with zero production violations
after the concurrent signature change was complete; one existing reasoned
stringified-error suppression remains.
Resumable records are canonicalized before their completion renderer, but their full
completion ACL/owner response and retry acceptance are still separate work.

## Move and appendable publication

The [move contract](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/move)
preserves the source's ACL and uploader ownership. Move now captures its
effective ACL before deleting the source, in the same transaction. A typed
publication mode preserves those entries without first reading or validating
the current default object ACL. This also preserves the virtual project-private
ACL of a legacy object without a stored ACL row. Unknown historical owners stay
unknown. Conditions, generation allocation, source deletion, destination
replacement, ACL storage, change journal and notification enqueue roll back
together when publication fails.

Appendable finalization now distinguishes reserved generation/timestamp metadata
from an unknown historical importer. A typed owner assignment preserves the
creator's actual owner ID when another authorized principal takes over and
finalizes. The finalizer's current access still controls publication. Current
registry aliases remain display data and do not transfer original ownership.
Copy, rewrite and compose assign their caller as the new uploader; restore keeps
the original uploader while inheriting the current default ACL in its supported
default-ACL mode.

The real-server cases are:

- `move_preserves_source_owner_and_acl_without_observing_current_default`:
  disk/RAM, distinct uploader and mover, exact source ACL/metadata/bytes, an
  otherwise oversized current default, failed source and destination conditions,
  disk ACL-write failure and transaction rollback, original-owner read access,
  denied access to the old destination generation and disk restart. A matching
  local notification configuration produces exactly Delete and Finalize outbox
  messages after success; the injected failure leaves the queue unchanged.
- `move_preserves_legacy_virtual_acl_and_unknown_owner_independently_of_current_default`:
  legacy absent ACL row, unknown historical owner, unchanged virtual grants,
  current-default isolation and exact bytes.
- `derived_publications_assign_the_caller_and_restore_keeps_original_owner_with_new_default_acl`:
  disk/RAM copy/rewrite/compose caller ownership, unchanged source ownership,
  restore authorization, preserved original uploader and current-default
  inheritance instead of the deleted generation's delegated OWNER grant.

Move uses its supported `projection=noAcl` response and an independent full GET
to verify actual stored ownership and ACLs. The existing `projection=full` move
rejection remains covered; successful full move responses are not implemented.

Catalog coverage adds:

- `appendable_finalization_preserves_creator_when_a_different_principal_takes_over`:
  disk/RAM creator/finalizer separation, unauthorized finalization, reserved
  generation/time, alias changes and reuse across restart, and denial after the
  former finalizer loses its grants.
- `appendable_owner_survives_power_loss_on_both_sides_of_finalization_commit`:
  independent Snapshot and PowerLoss cases before and after commit, plus
  simultaneous before/after PowerLoss arms, with restart, original owner,
  reserved generation and exact draft or finalized bytes.
- `overlapping_power_loss_arms_keep_their_own_baselines_and_all_seam_updates`:
  different arm-time baselines, reverse firing order and skipped hits, durable
  file/directory syncs, volatile creation, rename, retained unlink and truncation.

The initial simultaneous PowerLoss test exposed a test-model defect: only the
first armed ledger received durability updates, and snapshot firing selected a
ledger by directory order. The model now keeps each ledger on its own armed
fault and updates every matching active ledger. It therefore preserves each
arm's baseline even when firing order differs. Independent single-arm recovery
passed before that model fix; the failure did not demonstrate a production
fsync omission. The model still covers only its documented filesystem seams,
not physical power cuts. These publication changes add no filesystem write,
rename or synchronization step.

```sh
CARGO_INCREMENTAL=0 cargo check --locked --offline --lib --bin gcs-mimic --tests
CARGO_INCREMENTAL=0 cargo test --locked --offline --test acl_permission_effects ::owner_publication:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --test object_move_http --test acl_permission_effects -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::catalog::appendable::tests:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::catalog::fault::tests:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::catalog::admin::publication::tests:: -- --nocapture
CARGO_INCREMENTAL=0 cargo test --locked --offline --lib storage::resumable_http_tests::fault_injection::upload_ -- --nocapture
CARGO_INCREMENTAL=0 cargo xtask lint
CARGO_INCREMENTAL=0 cargo clippy --locked --offline --lib --bin gcs-mimic --test acl_permission_effects --test object_move_http -- -D warnings
```

The final publication checkpoint passed all three new real-server cases; the
whole ACL target passed 31 cases and the existing move target passed seven.
After the model fix, all 18 appendable cases, the new model case, 17 existing
administrative publication cases and both existing upload Snapshot/PowerLoss
cases passed. The strengthened final three-case wire run additionally checks
source and destination bytes, allocator, journal and outbox preservation after
condition and ACL-write failures. Compilation and target clippy passed;
production lint reported zero violations and one existing reasoned
stringified-error suppression. Independent source review found no major issue.
The optional offline Markdown lint attempt could not run because
`markdownlint-cli2` was absent from the local npm cache (`ENOTCACHED`); no package
was installed for that check. Full workspace, SDK and container runs remain
integration work.

## Known limits and follow-up scope

The following broader ACL acceptance work remains open. It is not a claim that
every feature-ledger row is part of the current migration request; additional
feature implementation requires a separately agreed scope.

- Canonical mirror representation for arbitrary local IAM IDs without an
  explicit alias remains unknown; their existing IAM grants are preserved.
- Owner propagation and ACL preservation for every publication/restore/move,
  import/export, appendable and fixture path; replay and completion snapshots
  must not reintroduce old display aliases. Selected-generation persistence and
  owner-preserving ACL mutations pass the checkpoint above, but the complete
  publication matrix is not yet verified.
- Full move responses remain rejected. Implementing them requires selected
  generation ACL-read admission in the mutation transaction, with corresponding
  permission and rollback negatives.
- Persisted upload identities still need current-registry rebinding with a
  typed original overwrite/delete capability ceiling; an expanded later role
  must not enlarge a session's originally admitted authority.
- Bounded trusted registry membership for group/domain/project teams; upload
  snapshots must not turn membership into broader IAM or trust request claims.
- Canned ACL effects, default ACL inheritance at every publication transaction
  and injected-clock UBLA 90-day locking boundaries.
- Protocol projection and full-resource echoes, permission changes during
  publication, disk/RAM restart and applicable fault/recovery boundaries.

No physical power-cut guarantee is claimed. Existing seam fault and PowerLoss
models cover only their modeled filesystem operations. No new filesystem
durability operation is introduced by the object-read slice. Full workspace,
SDK and container gates remain coordinated integration work.
