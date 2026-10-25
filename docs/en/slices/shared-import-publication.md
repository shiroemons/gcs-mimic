# Transactional filesystem import publication

Scope: DESIGN-072, DESIGN-073, DESIGN-074 and AUDIT-014. Parent rows stay
PARTIAL. Source decoding, seed adaptation and memory export are outside scope.

## Acceptance contract

- Publish staged file bytes with explicit imported generation, exact microsecond
  timestamps and required MD5/CRC32C. Require Replace and create-if-absent.
- Create imported buckets with their validated settings and owning project;
  retain default hold for future uploads without synthesizing imported holds.
- Commit publication, successful observations and cursor in one transaction.
  Batches use existing count/time bounds, SAVEPOINTs and owned cleanup.
- Retain successful elements around a rejected element. Persist observations
  only for successes, retain the cursor immediately before the first rejection
  and atomically mark the import terminal. Terminal imports remain unservable
  and cannot resume, revalidate or finish; no hole-filling is available.
- Persist bounded adapter failure details and expose progress for diagnosis.
  DiskFull and Busy advance nothing and preserve the active import marker.
- Before resume, new publication and completion, compare the entire catalog's
  bucket-plus-object count with the committed cursor, across all projects.
  The adapter additionally verifies each prefix identity and content digest;
  a matching count alone is not a complete destination identity proof.
- Atomically migrate schema 17 to 18, preserving existing data and rejecting
  failure/cursor inconsistencies; cover rollback, retry and reopen.

## Decisions

Schema 18 adds bounded terminal failure text to the journal. The active marker
remains set after failure. Terminal imports may have successful observations
beyond the contiguous cursor; only terminal imports allow that shape. Progress
exposes terminal state, while all mutating/resume operations reject it.
Failure details retain at most 256 Unicode characters (1024 UTF-8 bytes).
Use the existing staged handle and publication executor rather than another
file or SQL worker. No additional dependency or HTTP route is introduced.

Back up the complete stopped data directory before upgrading. Restore a matching
older executable with its entire backup to roll back; no in-place downgrade.

## Verification

On 2026-10-25 with Rust 1.99.1 on macOS ARM64, the targeted command below passed
34 tests: six import-publication cases, eight import-journal cases, five sync-state
cases and fifteen existing publication cases.

```sh
cargo test --locked --offline --lib -- import_publication import_journal \
  sync_state storage::catalog::admin::publication::tests
```

The six new cases cover preserved identity/settings/hold and atomic completion;
independent successes with a persistent terminal rejection; actual DiskFull with
no publication/observation/cursor advancement; foreign-project destination records
and bounded diagnostic persistence; Busy/cancelled-caller ownership and readable
committed bytes after restart; and schema rollback/retry preserving catalog and
journal identity. Existing sync/publication/journal tests remain unchanged in
behavior, with old-schema fixtures and current-version assertions advanced.

Workspace all-target clippy and format passed. The dependency freshness check
reported all pinned inputs CURRENT; no dependency changed. Independent review
found no release-blocking findings. Import adapter integration remains a separate
receiving gate. The coordinator
owns the final integrated-main `just check`; it is not duplicated here.
SDK, container and benchmark runs are excluded because this slice changes only
internal storage interfaces. No performance numbers are claimed.
