# Source state preservation

Scope: the filesystem-import subset of DESIGN-062, DESIGN-072, DESIGN-073 and
AUDIT-014. The feature ledger remains authoritative for parent status.

## Acceptance

- Preserve every storage class accepted by native object validation through the
  shared bucket/Autoclass class resolution, with no fallback to STANDARD.
- Preserve exact microsecond customTime, including its timezone spelling;
  empty and Go zero time remain unset. Reject malformed or lossy precision.
- Validate entity/role ACLs with native bucket/object rules, including duplicate
  entities and entry limits. Preserve explicit empty lists as empty ACLs.
- Commit an explicit object ACL in the same transaction as its generation,
  allocator and imported progress. Commit bucket ACLs with the bucket and progress.
- Recheck committed class, customTime and explicit ACLs before resuming.
- Reject ACL auxiliary identities or unknown fields that the target cannot
  represent, rather than dropping source information. Keep invalid imports from
  creating a destination.
- Keep UBLA, public access prevention, holds and retention checks in the shared
  publication path. Rejected ACLs must preserve existing bytes, ACL and allocator.

## Contract and limits

The pinned source is fake-gcs-server v1.56.1 at
`896ece51e49eed0946089f3089f6343855c0d106`. Google object metadata and ACL
semantics follow the [objects resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects).
No dependency was added; the verified Rust stable toolchain remains 1.99.1.

The filesystem reader still cannot reconstruct original names lost by source
path normalization or versions that the source filesystem never retained.
It preserves recoverable current state and leaves the source untouched.
Nonempty retention remains blocked because the source bucket format provides
no object-retention enablement flag; importing a configuration without an
enforceable bucket contract would be false success. Versioned memory snapshots
require their separate complete, quiesced export contract.

## Verification

The focused class/customTime case passes interrupted import, resume and two
catalog opens while preserving source metadata bytes and stat fingerprints.
The import unit suite passes ten cases, including explicit bucket/object ACL
restart preservation and rejection of invalid, duplicate or unrepresentable ACLs.
The publication rejection test covers invalid roles, UBLA and public access
prevention with unchanged object, ACL and generation allocator.

Full integration and production lint results are recorded after the final
source is available. SDK/container checks belong to the coordinator's final
integrated verification; no performance or power-loss result is claimed here.
