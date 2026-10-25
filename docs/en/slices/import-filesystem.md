# Durable filesystem import

## Scope and acceptance

This slice implements the local filesystem adapter for DESIGN-062,
DESIGN-072, DESIGN-073 and the source-cutover subset of AUDIT-014.
The pinned format and field mapping remain those in
[source-storage-contract.md](source-storage-contract.md).

- Preflight every metadata record before opening a new destination; preserve
  source bytes, attributes and permissions, empty buckets and normalized names.
- Publish verified file handles through shared administration with the source
  generation, MD5, CRC32C and exact microsecond creation/update times.
- Bind resume to the canonical source identity, selected options and ordered
  export fingerprint. Commit publication and contiguous progress together.
- Detect source changes between preflight, publication and a final metadata
  scan. Revalidate only committed bodies with inode/ctime drift on resume.
- Refuse serving unfinished imports, and finish import and seed atomically.
  Later seed settings must not replay data over imported objects.
- Stop visibly on checksum, metadata, binding, admission or disk errors;
  preserve committed progress and never update the journal in a separate TX.

The dedicated filesystem cases have passing native target tests. Existing
feature evidence is unchanged; parent rows remain PARTIAL until the complete
source-import contract passes.

## Shared boundary

The adapter uses shared typed bucket/file publication with imported identity
and journal updates in the same transaction. Schema 18 supplies permanent
failure state and checks destination counts across all projects. The adapter
additionally verifies each committed source/destination identity before resume;
it never writes SQL. See [shared import publication](shared-import-publication.md).

The adapter does not change server startup order, storage internals, seed MIME
or configuration. Unsupported non-STANDARD object classes are blocked until
shared imported publication can preserve their stored class.

## Execution and resume contract

Run the following against a stopped writer or immutable snapshot:

```sh
gcs-mimic import fake-gcs-server --source source-snapshot \
  --data-dir converted-data --metadata-mode sidecar --source-platform unix
```

Select `xattr` for a snapshot using extended attributes. The metadata mode and
source path platform are required; no automatic detection or fallback occurs.
The same invocation resumes a matching unfinished import. `--dry-run` retains
read-only inspection, and its optional `--verify-bodies` verifies checksums.
Actual import always verifies checksums and rejects `--verify-bodies` as an
inspection-only option. Server configuration and authentication profiles do not
change this trusted local administrative operation. Generated buckets use
`local-project`; no IAM or public permission is created.

Preflight scans only metadata and source stat. A private temporary file records
fixed-size per-element observations on the dedicated scanner thread. Publication
and final metadata scans compare those observations without retaining the whole
catalog. This file is outside both source and destination, uses a random name,
is removed at command exit and is never resume authority. A killed command can
leave this disposable temporary file; the next invocation builds a new baseline.
Only shared catalog/journal state determines committed progress.

Pages retaining opened source files contain at most eight records. With two
queued pages plus the consuming and constructing pages, at most 32 source files
are retained. Metadata-only and dry-run pages retain their 256-record limit.
The original opened files pass directly to staging; publication never reopens
their source paths.

The versioned source binding hashes the canonical UTF-8 source root, pinned
revision, fingerprint format and selected mode/platform/traversal/body limits.
The export binding hashes the existing
ordered metadata/stat-only manifest. Changed options or source location refuse
resume without modifying the original import state. A changed export under the
same source/options permanently fails that import, even if the source is later
repaired. Checksum corruption and unsupported source semantics also fail
permanently. Disk-full, admission, interrupted report output and stat-only drift
stop without automatic retry; matching progress remains available.

Before new publication, verify the committed destination prefix and revalidate
only committed bodies whose inode or ctime changed, using shared staging's
stored-hash checks and explicit discard. Unchanged committed bodies are not
reread. Source byte changes retaining all observable metadata remain outside the
immutable-snapshot prerequisite. Final metadata comparison gates completion.

This first adapter uses one-element publication batches to keep recovery,
terminal failure and ownership simple. This narrows the plan's batching design
requirement for the first useful local-development subset; shared publication
still owns its short transaction and atomic checkpoint. Larger batches and
staging overlap remain later work, without a performance claim.

Progress and completion are JSON Lines on stdout; findings and errors use stderr.
The preservation scope is recoverable current live disk state, including source
hashes/generations/microsecond times. Native ETags, metageneration, bucket times,
placement and owning project are generated. History, ACLs, retention, memory
exports, complete original-name recovery and reverse conversion are excluded.

Source immutability is required across the entire command. A change after the
scanner hands off an opened file can leave earlier batches committed before a
later staging/final-scan failure; the incomplete destination remains unservable.
Never remove only its marker. To abandon, stop all destination users, retain the
source snapshot/backup and replace only the disposable incomplete destination.

## Verification

Verified on native macOS ARM64 on 2026-10-25 with pinned Rust 1.99.1 and the
locked dependency graph. The official
[stable release notes](https://doc.rust-lang.org/stable/releases.html)
confirmed 1.99.1 at slice start. No dependency or toolchain was changed.

Target commands used `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`,
`CARGO_INCREMENTAL=0` and `RUST_TEST_THREADS=4`; these settings reduce build
storage without changing the checked behavior.

```sh
cargo test --locked --offline --test import --test import_filesystem
```

| Check | Result |
| --- | --- |
| Integration executables: import + import_filesystem | PASS, 15 + 12 tests. |
| `cargo test --locked --offline --lib import::` | PASS, 8 tests. |
| Existing `tests/import.rs` executable | PASS, 15 regression tests. |
| `cargo xtask compatibility validate` | PASS with runtime evidence updates. |
| `npx --yes markdownlint-cli2 docs/en/slices/import-filesystem.md` | PASS. |
| Final `just check` | PASS, 906 Rust/doc tests and all remaining checks. |
| Documented CLI example | PASS, 1 bucket, 1 object and 5 verified bytes. |

The new integration cases verify source bytes/stat/attributes, empty buckets,
hashes, generations, exact microsecond times, native metadata, default-hold
preservation and future uploads, interrupt/resume without rereading unchanged
committed bodies, inode replacement/chmod and identical xattr reattachment,
same-size source replacement during publication, terminal checksum/export drift
surviving source repair, foreign options and destination records, unsupported
ACL/class/precision blockers, unfinished serving, completed seed suppression,
CLI-only execution and generation URLs/conditions in strict/permissive profiles.
The descriptor regression imports 302 objects in a child with `ulimit -n 64`.

SDK, container, full onboarding-guide and benchmark runs are excluded because
HTTP behavior, container inputs and those guides are unchanged. The local CLI
example above reports `import_complete` and `bodies_verified: true`.
Host power loss, broad kill/ambiguous
commit injection, older-executable restoration, native Linux/Windows runtime,
non-STANDARD classes, multi-element staging overlap and memory/history export
remain unverified or unsupported. No performance claim is made.
