# SDK scorecard: case catalog

<!-- markdownlint-configure-file {"MD013": false} -->

Status: contract only. Nothing in this document is implemented yet.

Scope: the optional comparison harness under `sdk-scorecard/`. A case catalog
becomes the single place that defines every scorecard case, and the probes are
brought in line with it. No case is added, removed or renamed; no server
behavior, configuration or ledger status changes. The scorecard stays a
comparison, not a status source: `compatibility/features.json` remains the
only status ledger.

Out of scope, each a later slice: new shared cases, the shared gRPC set and
the move of Java's `sdk.grpc_*` cases to the `grpc` area, new SDK-specific
cases, and a gRPC column in `summary.md`.

## Problem

- What a case checks lives only in nine probes in seven languages. Shared IDs
  already check different things: the README's Limitations name two, and a
  review of the Go, Python and Ruby probes on 2026-11-15 found more (below).
- Each SDK's case count is written by hand in `run.sh` (`cases: 57`, `51`,
  `70`, `71`), and the Areas table in `sdk-scorecard/README.md` repeats them.
- `unsupported` markers are checked only for agreement between products. A
  probe that marks a supported case unsupported, or that reaches a private API
  to pass a case, is not caught.

Adding cases to nine SDKs on top of this would multiply each of these.

## Catalog

`sdk-scorecard/cases.json`, one document:

```json
{
  "sdks": ["go", "ruby", "node", "php", "python", "java", "dotnet", "rust", "cpp"],
  "cases": [
    {
      "id": "object.list_offsets",
      "area": "object",
      "sdks": "all",
      "checks": ["startOffset b and endOffset d over a, b, c, d list exactly b and c"],
      "unsupported": {"ruby": "Bucket#files in google-cloud-storage 1.62.2 has no start_offset or end_offset"},
      "variants": {},
      "features": ["BEHAVIOR-OBJECT-019"]
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `sdks` (top level) | The SDK keys in column order. |
| `id`, `area` | As today: one dot, lowercase; `area` is `bucket`, `object`, `signed`, `grpc` or `sdk`, and `sdk` exactly for IDs that start with `sdk.`. |
| `sdks` (case) | `"all"`, or the SDKs that emit the case. Core cases (`bucket`, `object`, `signed`) must be `"all"`. |
| `checks` | The observable effects every emitting SDK must assert, as short sentences. A probe may check more, never less. |
| `unsupported` | SDK key to the reason it has no public API. Only these SDKs may mark the case unsupported, and they must. |
| `variants` | SDK key to how that SDK reaches the same checks by a different request, for example PUT instead of PATCH. A variant may not weaken a check. |
| `features` | Ledger rows the case exercises, for readers; it changes no status. |

Rules the catalog encodes:

- The operation a case tests uses the SDK's public API. A probe may read a
  result through a non-public accessor only to assert, when the SDK exposes no
  public one, and the case's `variants` entry says so.
- Transport plumbing (CA, origin guard, redirect removal) may use any API; it
  is not a case.

## Harness changes

- `run.sh` derives each SDK's `cases` from the catalog and copies the catalog
  into the new results directory as `cases.json`, so every result directory
  states what its cases meant.
- `score.sh`, when the results directory holds `cases.json`, also checks:
  - the catalog itself: unique IDs, valid areas, the `sdk.` rule, known SDK
    keys, core cases on every SDK, `unsupported` SDKs among the emitting ones;
  - per SDK and product: the emitted ID set equals the catalog's, each area
    equals the catalog's, and `unsupported: true` appears exactly for the
    catalog's `unsupported` SDKs.
  Without `cases.json` it validates as today, so every recorded directory
  rescores byte for byte.
- `test-score.sh` adds a fixture catalog and a failing case for each new check.
- The Areas table in `sdk-scorecard/README.md` stays hand-written; a check in
  `test-score.sh` fails when its counts differ from the catalog.

## Probe alignment

Found by reading the probes on 2026-11-15. Each line is either aligned in this
slice or recorded as a catalog `variant`; nothing is left unstated.

| Case | Today | This slice |
| --- | --- | --- |
| `object.list_offsets` | Ruby calls the `@private` generated client, because `Bucket#files` has no offsets | Ruby `unsupported` (see Open questions) |
| `bucket.list_prefix_paging` | Ruby checks only the first page | Ruby checks every page with `next?`/`next` |
| `object.metadata_on_write` | Go checks one metadata key | Go checks the exact map |
| `object.copy_with_metadata` | Go and Ruby check the rewrite response; Python reloads the destination | All reload the destination and compare the exact map |
| `object.delete_not_found` | Go confirms deletion with a media read, the others with a metadata GET | Go uses a metadata GET |
| `object.update_metadata` | Go replaces and clears the whole map; the others delete one key | Go deletes one key if its public API allows; otherwise a variant (see Open questions) |
| `object.resumable_large` | Go also compares the returned CRC32C | Kept: stronger than the catalog's check; recorded as a variant |
| `object.checksum_rejected` | Python uploads 8 MiB + 1 because only its resumable path sends a supplied CRC32C; Go and Ruby use different upload paths | Variants |
| `bucket.update_labels`, `bucket.metageneration_precondition` | Ruby `Bucket#update` sends PUT; the others PATCH | Variant |
| Retention, soft delete and object retention settings | Go sets them at creation; Python and Ruby patch afterwards | Variant |
| `bucket.acl`, `bucket.default_object_acl`, `object.acl` | Go PUTs one entry, Python patches the list, Ruby POSTs | Variants |
| `object.compose` | Ruby reads `componentCount` through the `@private` `gapi` | Variant (assertion-only read) |
| C++ cases | `InsertObject` for small writes, deprecated checksum options, paging counted from the SDK's `rpc` log | Variants, as in its README |

Node.js, PHP, Java, .NET and Rust are reviewed against the catalog the same
way during implementation; anything found is aligned or recorded.

## Acceptance

| Case | Required behavior |
| --- | --- |
| CAT-SCHEMA | `cases.json` lists every case every probe emits today, in the Python probe's order for the core, and passes the catalog self-check. |
| CAT-RUN | `run.sh` has no hand-written case counts and writes `cases.json` into a new results directory. |
| CAT-SCORE | With a catalog, `score.sh` rejects a missing or extra ID, a wrong area, an unexpected or missing `unsupported` marker, and an invalid catalog; `test-score.sh` covers each. Every recorded summary still rescores byte for byte. |
| CAT-ALIGN | Every row of the alignment table is done or recorded as a variant, and every probe has been reviewed against every catalog check. |
| CAT-README | `test-score.sh` fails when the Areas table counts differ from the catalog. |
| CAT-RESULTS | All four products are rerun into a new results directory. Any score change is explained case by case; a case that newly fails because a check became stricter stays failed and is reported, not loosened. |

## Open questions

1. Ruby `object.list_offsets`: marking it unsupported follows the scorecard's
   rule and lowers Ruby to 50 scored cases (1 n/a). The alternative is a new
   rule that a documented generated client counts as public. Recommendation:
   unsupported.
2. Go `object.update_metadata`: if Go's public `ObjectAttrsToUpdate` cannot
   delete one key while keeping others, keep the whole-map check as a variant
   rather than drop the case. To confirm against v1.70.0 at implementation.
3. Whether `variants` may stay free text or should be enumerated. Recommendation:
   free text now; revisit when slice C adds cases.

## Verification

To be recorded when implemented.
