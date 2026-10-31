# Explicit local instance reset

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the reset subset of DESIGN-068. The separate [atomic replay slice](atomic-seed-reseed.md)
adds reseed and the source helper alias; [process-local storage](memory-backend.md)
owns the RAM backend. None is a Google API operation. The compatibility ledger retains the broader incomplete cases.

## Accepted boundary

The reset contract was agreed before implementation: an idle instance may be
reset atomically; active work returns 503 without mutation. Active directory
synchronization and incomplete import return 409. The operation never follows
or modifies a seed, import-source or sync directory. An already opened immutable
reader may finish with its selected bytes.

The reference is fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`, with its
[internal management routes](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go).
Explicit confirmation, startup authority, strict local-manager selection and busy
rejection are local safety choices. They are not provider-exact error claims.

## Request and authority

Enable local management explicitly with `--admin-enabled true`. Catalog open
creates the private management capability; requests only use the stored grant.
Strict mode also requires a registered bearer principal whose ID exactly matches
`--admin-principal ID`. Google IAM roles do not confer this authority, and the
manager receives no Google API permissions. Isolate the enabled listener.

Send `POST /_internal/reset` with the JSON body:

```json
{"confirm":"gcs-mimic"}
```

The confirmation is mandatory. Wrong methods return 405 with `Allow: POST`;
query parameters, unknown fields, duplicate confirmation fields and invalid or
missing confirmation return 400. The existing 64 KiB JSON cap applies. Disabled
management returns 404. Authentication and manager selection precede body parsing.

The source-compatible `POST /_internal/delete_all` alias accepts only an empty
body and no query parameters. It uses this same authority, reset gate and
transaction; the native `/_internal/reset` confirmation rule stays unchanged.

A successful response has `buckets`, `objects` and `cleanupPending`. Object counts
include noncurrent and soft-deleted generations. For example:

```json
{"buckets":1,"objects":2,"cleanupPending":false}
```

## Quiescence and transaction

One shared Tokio read/write gate protects catalog activity. SQL jobs retain owned
read leases through their executor completion, including cancellation. Upload
admission, resumable/rewrite/derived writers, unconsumed derived sources, staged
administrative files, appendable writers and asynchronous reclamation retain
leases while unpublished bytes or cleanup can still affect state. Reset acquires
an exclusive lease with `try_write_owned`; a busy gate returns immediate 503 with
`Retry-After: 1`. New activity cannot enter between its idle check and reset.
No polling loop, body buffering, forced cancellation or long database transaction
is introduced. Existing immutable read descriptors are intentionally independent.

The short immediate transaction:

1. Rejects an active import without changing its journal or objects.
2. Inserts object, session, derived and appendable opaque file IDs into the existing deletion journal with `INSERT SELECT`; no whole catalog or object body is collected.
3. Clears buckets, all object versions, IAM policies, ACLs, notification configurations, HMAC keys, resumable/rewrite/derived/appendable state, invocation replays, import records, sync bindings/work-back state/findings and change-journal records. Bucket and object ACL/notification rows use their existing foreign-key cascades.
4. Clears seed completion and advances the change-journal epoch to invalidate old observations.
5. Preserves catalog identity, schema, object-generation and policy-revision high-water marks, the injected clock, authentication registry, parent-policy fixture, startup configuration and runtime endpoint settings.

The epoch update commits with deletion and resolves an ambiguous commit result:
an error reported after a successful commit does not pretend that data remains.
Generation and policy revisions of recreated resources advance rather than
reusing the preceding ordinary identity.

The exclusive lease stays owned by the executor after a dropped response wait.
As with other completed mutations, a deadline or disconnect after submission may
leave an unknown client outcome; inspect instance state before retrying.

## Files, failure and recovery

After commit, reset removes journaled opaque entries from blob, staging and
appendable namespaces and syncs their parent directories. It processes at most
128 IDs per page on the existing bounded catalog worker. It never constructs a
path from a bucket or object name and never traverses external source/sync paths.

Failures before commit preserve the previous metadata and bytes. A physical
reclamation failure after commit cannot undo the reset: success reports
`cleanupPending: true`, logs `reset_cleanup_deferred` and retains the deletion
journal. Startup recovery reclaims those unreferenced entries before readiness.
Open reader descriptors may finish; platforms that defer unlinking retain cleanup
work for recovery.

This operation clears instance state, not the data directory or mounted volume.
Restart still follows normal startup rules: configured fixtures may be replayed
and configured bucket initialization may create missing buckets. Reset itself
does not replay fixtures. The separate [reseed endpoint](atomic-seed-reseed.md)
validates and stages the complete configured fixture tree before publishing it.

## Acceptance and verification

Storage tests in `src/storage/catalog/admin/reset/tests.rs` exercise revision and
catalog-identity preservation, open-reader completion, busy upload/source leases,
new-activity rejection under the exclusive lease, incomplete-import refusal,
failed and ambiguous commit, deferred cleanup and restart. Snapshot and Unix
power-loss-model cases cover before/after commit and the new unlink/sync seams.
The existing power-loss model does not undo unlinks; these cases do not establish
complete power-loss namespace durability.

Wire tests in `tests/admin_reset/mod.rs` cover default-off/confirmation errors,
session invalidation, stale generation and policy identity after recreation,
restart, an active `100 Continue` upload, strict manager/IAM separation and active
sync refusal with preserved external bytes. All execute offline on loopback.

Native verification on 2026-10-30: `cargo check --locked --offline --tests`
passed. `cargo test --locked --offline --lib storage::catalog::admin::reset`
passed all six cases, and the broader
`cargo test --locked --offline --lib storage::catalog::admin` passed 55 cases.
`cargo test --locked --offline --test admin_http` passed all 17 cases before
the saved-prefix/history extension, then the three reset wire cases passed
with that extension. `cargo xtask lint` reported zero production violations;
both local-administration slice documents passed Markdown lint.
SDK, container, native Windows and real host-power-loss checks belong to
separately recorded integration or platform work. There is no benchmark claim.

## Extension rule

Any later persistent feature must add its state and byte namespaces to this reset
transaction and its recovery tests. Schema 37 includes XML multipart sessions,
parts and deferred part deletions: reset journals their opaque staging IDs before
clearing parts, sessions and cleanup records. Multipart stream/completion leases
keep reset busy until their owned work finishes. The multipart wire and storage
tests exercise session invalidation, staging reclamation, restart and the busy
boundary. Current reset also clears notification outboxes and the later shared
synchronization state. [Atomic reseed](atomic-seed-reseed.md) reuses the same
transaction clearing rules, while validating and staging input before any clear.
