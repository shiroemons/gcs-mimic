# Atomic fixture replay and source management helpers

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the whole-tree publication and direct-fixture subset of DESIGN-062 and DESIGN-068. Both broad feature scopes remain incomplete; the central ledger owner reconciles their status. These are opt-in emulator management operations, not Google API methods.

## Contract and source boundary

The reference is fake-gcs-server v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`. Its pinned [server implementation](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go), git blob `da158fa063131002fa06dc43d4a26c4f250f6681`, registers POST/PUT `/_internal/reseed` and POST `/_internal/delete_all`. Reseed reads the configured Seed directory; its filesystem backend merges fixtures, while its memory backend constructs a replacement namespace. The [source manifest](../../../compatibility/upstream/source-storage-contract.json) retains the other source identities. Inspection was refreshed on 2026-10-30 UTC.

Google's current [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects), [object ACL resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objectAccessControls), and [retention configuration](https://docs.cloud.google.com/storage/docs/object-lock) govern ordinary metadata, permission restrictions and protection. Explicit creation times and generations remain local fixture extensions. No Google seeding/reset method is inferred from Discovery presence. Existing Google compatibility gaps remain open.

Seed input stays a literal directory of bucket/object files. A regular `value.metadata` is an object containing its literal bytes. Durable source xattrs/sidecars belong to the separately selected import format; this slice adds no sidecar detection, new input format or configuration option.

## Atomic namespace publication

Startup replay and disk reseed preserve existing bucket settings and all objects without a corresponding fixture file. A differing fixture replaces the selected live object under the same catalog transaction as the rest of the tree; versioning, holds, retention, hashes and generation allocation use shared publication. Identical fixture bytes and seed-default content metadata keep the existing generation, including an identical held object.

RAM reseed replaces the namespace. It uses the reset transaction's clearing rules and preserves catalog identity and allocator high-water marks. Existing protection and unfinished import/synchronization state prevent replacement. Old immutable reader references remain readable after a successful replacement.

The loader validates and stages the complete input before one immediate SQL transaction publishes its buckets, object references and seed-completion state. It never resets the catalog and then uploads objects one at a time. A malformed later fixture, checksum mismatch, capacity failure, protection failure or failed precommit operation leaves the preceding catalog and bytes intact, including any other object or empty bucket selected by that failed batch.

This intentionally changes the previous startup protection behavior: a differing held/retained fixture now fails the entire replay and startup emits no ready event. It no longer increments `blocked` and continues with the rest of the tree. The diagnostic explains that the entire seed was left unapplied and directs the operator to start without `seed_dir` to release a hold, or wait for retention expiry. Successful outcomes retain the existing JSON count fields; `blocked` is zero.

Disk staging uses opaque immutable blob IDs, same-body hashing and the existing durable file/shard publication protocol. No SQL transaction spans a body transfer. An ambiguous commit is checked against committed references and the reset epoch before returning success. Cleanup probes references before removing bytes, so a lost response never authorizes deletion of a committed blob. Unreferenced stages/orphans remain recoverable through ordinary startup recovery if cleanup itself fails.

RAM staging uses the existing process-local Store and its byte budget. Old and new bodies, descriptors and reader-held chunks remain charged until their references drop; there is no temporary-disk fallback. RAM reset does not grow a disk deletion journal: after confirmed commit, the same owned job retains only the bounded prepared namespace's IDs and clears old drafts before releasing the exclusive gate. A full-page-catalog regression verifies replacement, old readers, new fixture bytes and subsequent API writes.

## Admission, cancellation and source safety

Traversal retains the existing two-slot channel, 10,000-entry total and 32-level depth bounds. A batch accepts at most 1,000 buckets and accounts at most 8 MiB of retained metadata. Source reads and staging use fixed 64 KiB buffers on the existing bounded file workers. Holding a single batch admission lease avoids deadlocking a tree larger than the ordinary 256 staged-handle budget.

Batch cancellation transfers all remaining entries to one supervised sequential cleanup owner. Preparation work retains its current entry until filesystem work finishes; SQL work owns the complete batch through commit confirmation. Catalog close waits for shared ownership to drain. Cancellation before SQL admission publishes nothing. Once the owned SQL operation is running, disconnect or a response timeout can leave a completed whole batch; callers must inspect state rather than infer rollback from a missing response. No partial namespace is returned as success.

The publication boundary takes the same exclusive activity gate as reset, protecting notification/reset activity and unpublished uploads. A competing admitted operation returns a retryable busy failure. Runtime synchronization and incomplete import state are rejected; management never resets or edits external seed/import/sync files.

On Unix, the loader pins the original root descriptor and checks its device/inode against the inspected directory before canonicalization, then opens every ancestor and leaf with the existing no-follow descriptor helpers. Deterministic root/leaf/ancestor symlink swaps cannot read or publish an external canary. Windows retains its prior trusted, stable read-only source requirement and path-opening behavior; the Unix concurrent-swap guarantee is not claimed there.

## Management wire contract

All helpers are disabled by default and require startup-issued local administrative authority. In strict mode only the configured `admin_principal` may call them; Google `roles/storage.admin` alone is insufficient, and the local manager receives no Google API grant. Public XML virtual-host object names resembling management routes retain object semantics.

- POST or PUT `/_internal/reseed` accepts an empty body and no query parameters. It uses only the effective startup `seed_dir`, returns the existing seed count object, and fails if no seed directory is configured. Disk merges; RAM replaces. The caller cannot supply another path or mode.
- POST `/_internal/delete_all` accepts an empty body and delegates to the existing reset implementation, with the same busy/import/sync guards and high-water preservation. Other methods return 405. The native `/_internal/reset` endpoint still requires its existing explicit confirmation JSON.
- Bodies are bounded to 64 KiB before parsing/rejection. Nonempty bodies and query parameters are rejected; no ignored helper fields are accepted.

Direct POST `/_internal/objects` now accepts ACL, object retention, temporary hold and event-based hold metadata through the existing shared validators. ACL replacement commits with the object and enforces UBLA/PAP; retention uses enabled-bucket and current-time restrictions. ACL preservation covers entity and role; unmodeled nonempty identity decorations and unknown nested fields are rejected rather than silently dropped. Empty/null source-codec placeholders remain accepted. Bad ACL roles, invalid retention/timestamps, generation conflicts and checksum failures preserve the previous object and queued notifications. Deleted/archive identity injection, caller-supplied ETags and sizes remain explicitly rejected. The [direct fixture contract](local-fixture-administration.md) retains its other framing, timestamp and authority rules.

The Go facade forwards ACL and retention through its existing source JSON codec to this same endpoint. Missing buckets needed by a retention-bearing fixture use the official Go client's `SetObjectRetention(true)` on creation; existing buckets are never upgraded. Constructor precreation makes a plain fixture followed by a retained fixture in the same bucket independent of order. Ordinary CreateObject writes retain their notification behavior; startup and reseed fixtures remain quiet.

## Verification

The following targeted checks passed on the development host; they are not SDK, container, Windows or real-cloud evidence:

| Command | Result and observable scope |
| --- | --- |
| `cargo check --locked --offline --tests` | PASS, unfiltered output |
| `cargo test --locked --offline --lib seed::tests` | 13 passed: the ten reseed tests, complete/incomplete walk and deterministic root/leaf/ancestor canary exclusion; includes full RAM catalog replacement, true byte-budget exhaustion, old readers/identity/unrelated objects, 300-entry cancellation and close drain, cancellation before SQL, active-upload exclusion, held/unfinished-import rollback, copy/write/flush/sync/publication/commit failures and ambiguous commit |
| `cargo test --locked --offline --lib -- seed::tests retained_namespace admin::publication::tests admin::reset::tests` | 37 passed: the above, reader/chunk budget release, existing publication modes/admission/shutdown/recovery and existing reset invariants |
| `cargo test --locked --offline --test admin_http` | 24 passed: source helper methods/bounds/strict authority, disk/RAM lifecycle, literal metadata-suffix object, quiet reseed with unchanged later runtime event selection, protection rollback, reset alias and virtual-host collisions, direct ACL/hold/retention and existing admin regressions |
| `cargo test --locked --offline --test seed --test admin_http` | 21 seed and 24 admin cases passed together, including owned SQL shutdown after the debug-only commit-start signal, hold rollback without readiness, migration and source-literal regressions |
| `cargo xtask lint` | Zero production violations |
| `npx --yes markdownlint-cli2 docs/en/slices/atomic-seed-reseed.md docs/en/slices/seed.md docs/en/slices/local-fixture-administration.md docs/en/slices/local-instance-reset.md docs/en/slices/go-subprocess-facade.md` | Zero issues |
| `go build ./...`; `go test -race ./fakestorage`; `go vet ./...` from `go/` | PASS: metadata codec/forwarding and native facade tests; no real Rust/official-SDK runtime claim |
| `go test -tags integration -run '^$' ./fakestorage` from `go/` | PASS, compile only; `TestRustFixtureACLAndRetention` is reserved for final-source integration |

The crash test runs process-abort and `Fault::PowerLoss` snapshots at BeforePublish, AfterPublish, BeforeCommit and AfterCommit, reopening each snapshot and requiring exactly the old or new complete namespace. The model covers the existing seamed durability steps; it is not proof of physical host power-loss behavior or every instruction window.

A separate static review covered cancellation ownership/fanout, original-root identity, full RAM catalog replacement, exclusive-gate pruning and Go metadata forwarding. Its requested corrections were applied and it reported no remaining major findings. That reviewer did not rerun these commands.

No dependency was added. Actual official-SDK/container/browser suites, cross-platform source races, performance measurements and direct archived-generation fixtures were not run or implemented by this slice. The final integration owner owns the central ledger and combined distribution/SDK gates.
