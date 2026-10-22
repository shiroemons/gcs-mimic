# Bucket metadata PATCH final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Integration date: **2026-10-23**. Final verification is pending the reviewed SDK
branch tip; this draft records no unexecuted command as passing.
The [contract](../slices/bucket-metadata-patch.md) freezes the supported subset,
test IDs, input/profile rules and remaining Google service questions.
The [comparison](bucket-metadata-patch-comparison.md) records the pinned reference
and official SDK serialization separately from whole-method compatibility.

## Scope and acceptance

Labels merge, key-null removal, whole-map null clearing and empty-map preservation;
supported default class; versioning/default holds and their future object effects;
atomic match/not-match conditions; metageneration/etag/clock changes; strict update
permission, transaction-time revocation and durable GET/list consistency.
The feature ledger decomposes these cases and retains pending advanced-field,
ACL/billing/full-projection and provider-parity cases. JSON-BUCKETS-008 remains
PARTIAL. No whole-method promotion follows from a scoped workflow checkmark.

## Final verification

Commands, raw output, toolchain/container provenance and delivery completion
conditions will be recorded after the reviewed final source is merged.
No real-cloud resources, release publication, dependency update or new performance
claim belongs to this slice. The next slice is bucket metadata PUT, order 7,
with independently reconciled replacement/default semantics.
