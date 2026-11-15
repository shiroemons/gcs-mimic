# SDK scorecard: case catalog

<!-- markdownlint-configure-file {"MD013": false} -->

Status: implemented on 2026-11-15; see "Verification".

Scope: the optional comparison harness under `sdk-scorecard/`. A case catalog
becomes the single place that defines every scorecard case, and the probes are
brought in line with it. No case is added, removed or renamed; no server
behavior, configuration or ledger status changes. The scorecard stays a
comparison, not a status source: `compatibility/features.json` remains the
only status ledger.

Out of scope, each a later slice: new shared cases, the shared gRPC set and
the move of Java's `sdk.grpc_*` cases to the `grpc` area, new SDK-specific
cases, a gRPC column in `summary.md`, and ledger references per case.

## Problem

- What a case checks lived only in nine probes in seven languages, and shared
  IDs checked different things: the README's Limitations named two, and the
  review below found more.
- Each SDK's case count was written by hand in `run.sh` (`cases: 57`, `51`,
  `70`, `71`), and the Areas table in `sdk-scorecard/README.md` repeated them.
- `unsupported` markers were checked only for agreement between products. A
  probe that marked a supported case unsupported, or that reached a private API
  to pass a case, was not caught.

Adding cases to nine SDKs on top of this would multiply each of these.

## Catalog

`sdk-scorecard/cases.json`, one document:

```json
{
  "sdks": ["go", "ruby", "node", "php", "python", "java", "dotnet", "rust", "cpp"],
  "notes": {"rust": "Bucket operations ... go through the StorageControl gRPC client ..."},
  "cases": [
    {
      "id": "object.list_offsets",
      "area": "object",
      "sdks": "all",
      "checks": ["over a, b, c and d, startOffset b and endOffset d list exactly b and c"],
      "unsupported": {"ruby": "google-cloud-storage 1.62.2 has no public API for listing with start or end offsets (Bucket#files takes neither)"}
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `sdks` (top level) | The SDK keys in column order. |
| `notes` | SDK key to a difference that covers that whole SDK, so it is not repeated on every case. |
| `id`, `area` | One dot, lowercase; `area` is `bucket`, `object`, `signed`, `grpc` or `sdk`, and `sdk` exactly for IDs that start with `sdk.`. |
| `sdks` (case) | `"all"`, or the SDKs that emit the case. Core cases (`bucket`, `object`, `signed`) must be `"all"`. |
| `checks` | The observable effects every emitting SDK must assert, as short sentences. A probe may check more, never less. |
| `unsupported` | SDK key to the exact reason the probe emits. Only these SDKs may mark the case unsupported, and they must, with that text. |
| `variants` | SDK key to how that SDK reaches the same checks by a different request, for example PUT instead of PATCH. A variant may not weaken a check. Free text. |

Rules the catalog encodes:

- The operation a case tests uses the SDK's public API. A probe may read a
  result through a non-public accessor only to assert, when the SDK exposes no
  public one, and the case's `variants` entry says so.
- Transport plumbing (CA, origin guard, redirect removal) may use any API; it
  is not a case.

## Harness

- `run.sh` checks the catalog with `score.sh --check-catalog` before building
  anything, requires its `sdks` to be the nine SDKs it runs in order, derives
  each SDK's `cases` from it and copies it into the results directory as
  `cases.json`.
- `score.sh`, when the results directory holds `cases.json`, also checks:
  - the catalog itself: distinct SDK keys and case IDs, valid IDs and areas,
    the `sdk.` rule, core cases on every SDK, known SDKs in every case list,
    non-empty checks, `unsupported`, `variants` and `notes` only for SDKs that
    emit the case or exist, with non-empty text;
  - that the catalog's SDKs are the recorded SDK set;
  - per SDK and product: the emitted ID set, each area, and each unsupported
    reason equal the catalog's. The diff prints one case per line.
  Without `cases.json` it validates as before, so every recorded directory
  rescores byte for byte.
- `test-score.sh` adds a fixture catalog: scoring with it changes no output,
  seven changes to it (missing case, wrong area, missing, different and
  unexpected unsupported reasons, wrong SDK list, missing SDK) fail scoring, six
  invalid catalogs fail both scoring and `--check-catalog`, and the Areas table
  in `sdk-scorecard/README.md` must count what the repository catalog defines.

## Probe alignment

The Go, Python and Ruby probes were compared case by case; the Node.js, PHP,
Java, .NET, Rust and C++ probes were reviewed against every catalog check by
reading them. Each finding is either fixed in a probe or recorded as a
`variant`; nothing was loosened in the catalog to make a probe pass.

Fixed in the probes:

| Case | Before | Now |
| --- | --- | --- |
| `object.list_offsets` | Ruby called the `@private` generated client, because `Bucket#files` takes no offsets | Ruby emits it `unsupported` |
| `bucket.create_get_delete` | Ruby and PHP checked only that a lookup returned something | Both compare the fetched bucket's name |
| `bucket.list_prefix_paging` | Ruby checked only the first page | Ruby checks every page with `next?`/`next` |
| `bucket.lifecycle`, `bucket.cors` | Node compared partially, so extra rules passed; PHP did not count CORS rules | Both require exactly one rule |
| `bucket.autoclass` | Node and PHP enabled it with a PATCH after creating the bucket | Both create the bucket with autoclass, as the other probes do |
| `object.metadata_on_write` | Go checked one key; Node compared partially | Both compare the exact map |
| `object.copy_with_metadata` | Go and Ruby checked the rewrite response; Node compared partially | All reload the destination and compare the exact map |
| `object.delete_not_found` | Go confirmed deletion with a media read | Go reads the metadata |
| `object.customer_supplied_key` | Go, Node, Java and C++ accepted any failure of the keyless read, including a transport error | All require an HTTP error response, as Python, Ruby, PHP, .NET and Rust already did |

Recorded as variants (62 entries) and one SDK note:

- Rust performs bucket and metadata operations and listings through
  StorageControl gRPC (`notes.rust`), and reads ACL lists and saves them whole.
- ACL entries are set in four ways: Go PUTs one entry; Python, Java and .NET
  save the whole list with a PATCH; Ruby, Node.js, PHP and C++ insert with a
  POST.
- Go's public API cannot delete one metadata key, so its `object.update_metadata`
  checks the whole-map merge and clear instead of individual-key deletion.
- Checksum and MD5 upload paths differ (Python's 8 MiB + 1 resumable upload,
  Java's `WriteChannel`, Node.js and PHP metadata fields, C++'s deprecated
  options, Rust's single request).
- Go sets retention and soft-delete settings at creation; Ruby's bucket update
  is a PUT; PHP signs through `bucketBoundHostname` and sends ranges and
  `Accept-Encoding` through `restOptions`; Rust's default read returns stored
  gzip bytes; .NET composes through the generated `Google.Apis.Storage.v1`
  service; Ruby reads `componentCount` through `@private` `gapi`; C++ counts
  list pages from its `rpc` log.

The SDK-specific `sdk.*` checks were reworded where they stated less than the
cases assert, for example the Java Transfer Manager cases now name the
composite and ranged-request checks they make.

## Acceptance

| Case | Required behavior |
| --- | --- |
| CAT-SCHEMA | `cases.json` lists every case every probe emits, in the Python probe's order for the core, and passes the catalog self-check. |
| CAT-RUN | `run.sh` has no hand-written case counts and writes `cases.json` into a new results directory. |
| CAT-SCORE | With a catalog, `score.sh` rejects a missing or extra ID, a wrong area, an unexpected, missing or reworded `unsupported` reason, and an invalid catalog; `test-score.sh` covers each. Every recorded summary still rescores byte for byte. |
| CAT-ALIGN | Every finding is fixed or recorded as a variant, and every probe has been reviewed against every catalog check. |
| CAT-README | `test-score.sh` fails when the Areas table counts differ from the catalog. |
| CAT-RESULTS | All four products are rerun into a new results directory. Any score change is explained case by case; a case that newly fails because a check became stricter stays failed and is reported, not loosened. |

## Decisions

1. Ruby `object.list_offsets` is unsupported: `Bucket#files` takes no offsets,
   and counting a call through the `@private` `Project#service` would break the
   rule the other SDKs' `n/a` markers follow. Ruby has 50 scored cases (1 n/a).
2. Go `object.update_metadata` keeps its whole-map check as a variant: the
   public `ObjectAttrsToUpdate` of v1.70.0 sends a non-empty map as a merge and
   only an empty one as `null`, so one key cannot be deleted.
3. `variants` stay free text; revisit when slice C adds cases.

## Verification

To be recorded with the four-product results.
