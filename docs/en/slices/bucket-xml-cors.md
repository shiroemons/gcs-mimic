# Bucket configuration and XML read CORS

<!-- markdownlint-configure-file {"MD013": false} -->

This slice implements BEHAVIOR-BUCKET-009 and DESIGN-052 for persisted JSON
bucket configuration and existing path-style XML object GET/HEAD. It keeps JSON
API defaults independent. XML `?cors` configuration operations, XML writes and
signed transfers remain outside this slice.

## Sources checked on 2026-10-23

- [Cross-origin requests](https://docs.cloud.google.com/storage/docs/cross-origin)
  defines endpoint separation, matching, preflight headers, default 3600-second
  caching, absent XML credential support and exposed headers.
- [CORS examples](https://docs.cloud.google.com/storage/docs/cors-configurations)
  includes subdomain origins such as `https://*.example.com` and clearing rules.
- [JSON bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets)
  defines `cors`, its string lists and integer `maxAgeSeconds`; methods can use
  `*` and the JSON schema also permits `OPTIONS`.
- [Bucket listing](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list)
  permits fewer items than `maxResults`; clients must follow `nextPageToken`.
  The continuation identifies the last returned bucket name, not the lookahead.
- [Set bucket metadata](https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata)
  specifies the first origin/method matching rule. A header rejection in that
  rule does not fall through to a later rule.
- Discovery revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`,
  supplies the same writable CORS fields; see the committed source manifest.
- Storage v2 protobuf blob `f56e4f3e0a0795ae8dc45f322477ee14b96d5291`
  defines optional string lists, method/origin `*` and signed int32 max age;
  the [shared CORS contract](cors.md#official-inputs) records its verified digest.

The configuration schema and runtime XML method list are distinct: storing an
`OPTIONS` entry does not require `OPTIONS` in order to authorize preflight for
GET/HEAD. No XML write implementation is implied by a configured write method.
Provider-exact validation messages, malformed-origin precedence and response
header formatting have not been established by real-cloud differential tests.

## Acceptance contract

| Area | Supported behavior and verification |
| --- | --- |
| Create/get/list | JSON create persists ordered CORS rules. Bucket resources expose nonempty rules and omit an empty configuration. Unknown rule fields follow existing ignored-input behavior. |
| PATCH | Omitted `cors` preserves rules; a supplied list replaces all rules; `[]` or `null` clears them. |
| PUT | Supplied rules replace all rules; omitted, `[]` or `null` clears them, matching the existing modeled-field replacement profile. |
| Schema | Rules are objects; origin/method/responseHeader are optional string arrays; maxAgeSeconds is an optional nonnegative int32. Empty rules/list fields have no match. Empty/whitespace/control-bearing origins, invalid HTTP method/header tokens and negative/overflow/fraction/string max age fail with 400 and preserve the entire bucket. |
| Limits | Existing bounded JSON metadata bodies and the 64 KiB stored CORS limit remain unchanged, including empty rules. Expanded bucket-list pages use the budget below, independently of `fields`. Catalog concurrency, SQLite cache and queues retain their existing limits. Object bytes never enter this metadata path. |
| Permissions | Existing strict `storage.buckets.create`, `storage.buckets.get` and `storage.buckets.update` checks apply. Update authorization and metageneration conditions run in the same catalog transaction as CORS mutation. |
| Persistence | Schema 12 adds a nonnull JSON column with `[]` for older buckets. Migration is transactional. Restart preserves rules, order, optional max age and other metadata. |
| Concurrent updates | Eight updates using one metageneration produce one success and seven precondition failures. Failed parse/permission/condition checks never change CORS or other metadata. |
| XML matching | Exact or whole-wildcard origins and methods; `https://*.example.com` matches nonempty subdomains with the same scheme and effective port, without matching the apex or another domain. Documented default ports compare equal to omission; custom schemes infer no default port. First matching origin/method rule wins. |
| XML simple GET/HEAD | Matching rules echo the request origin, expose configured response headers and vary by origin. No rule or no match returns no CORS approval. Non-CORS reads retain normal behavior. |
| XML preflight | OPTIONS does not require object permission. The requested method must match; requested header names must all match the same rule, case-insensitively, or a configured `*`. Valid requests return 200, allow methods/headers and configured max age or 3600. Mismatches/malformed or duplicate CORS inputs return 200 without approval. |
| Credentials and authorization | XML never sends Access-Control-Allow-Credentials. Allowed preflight cannot grant object read access; actual reads still enforce IAM and retain 403/404/412/416 responses. |

The `null` clearing rule, omitted PUT behavior and exact malformed-input errors
are declared local interpretations extending the existing metadata contract.
They are not claims of complete provider wire parity. Origin configuration is
stored as strings; it is not used to construct URLs, alter permissions or select
the advertised endpoint.

## Bounded bucket list pages

Before decoding a row, the catalog charges 4096 bytes plus 8 times the UTF-8
bytes of name/project/location/locationType/storageClass, 16 times stored label
JSON bytes and 64 times stored CORS JSON bytes. Borrowed SQLite text is inspected
without allocating Rust rule arrays or JSON values. The factors estimate retained
typed metadata and response storage, including allocation capacity; they do not
bound the full transient parsing/validation peak or measure an allocator limit. The fixed charge
covers the record, response keys, timestamps, token and ordinary advertised URL.
The budget is 256 KiB, so 32 ordinary concurrent pages charge at most 8 MiB in
total. Runtime memory, SQLite's 2 MiB cache and transient catalog allocations
are additional; this charge total is not a process-memory guarantee.

If the next row would exceed the remaining charge, it is left unexpanded and
the nonempty page receives a continuation token for its last returned name.
The count cap uses the same unexpanded lookahead. SQL still seeks by project,
prefix and exclusive last-name bound, and reads at most the item cap plus one
row. Changing `maxResults` or response `fields` between pages cannot skip a row.
Field selection may remove `nextPageToken`; callers requesting partial responses
must explicitly select it to enumerate all pages.

A first row exceeding the budget is returned alone. This exception is necessary
to preserve all accepted metadata and ensure forward progress. For the dense
empty-rule case, the unchanged 64 KiB CORS limit permits at most 21,844 rules:
with Rust 1.99.1 on a supported 64-bit target, the fallible typed-rule collection
has capacity 32,768, occupying 2,621,440 bytes at 80 bytes per slot. The response
Value array occupies another 699,008 bytes at 32 bytes per slot. Together these
arrays occupy 3,320,448 bytes, about 3.17 MiB per singleton or 101.33 MiB at 32
slots, before other allocations. These capacity measurements are not process RSS.
Input Value arrays, validation Values and serialization buffers add transient
memory in the single catalog execution lane. For example, 1,025 empty rules
produce 262,176 bytes of simultaneously live input/typed/validation array
capacity, exceeding their 196,864-byte CORS charge. Response serialization drops
the typed page before writing the body. A 20,000-rule configuration is 60,001 serialized bytes, with no rule loss
or normalization. These sizing estimates do not replace a 128 MiB cgroup replay
or establish a universal RSS ceiling for arbitrary library-created records,
allocator behavior, unusually large configured endpoints or mixed workloads.

Behavioral regression tests create 60 such accepted buckets, then enumerate
name-only and full resources in both compatibility profiles after restart,
checking every name once and all 20,000 rules. Separate catalog cases place
small records before and after singleton pages and check count/empty/final
cursor boundaries. The Google listing allowance above was checked on
2026-10-23; these resource charges are emulator choices, not provider limits.

## Verification entry points

- `cargo test --lib xml::cors::tests`
- `cargo test --test bucket_http bucket_cors`
- `cargo test --test catalog`
- `cargo test --test bucket_http bucket_cors_large_metadata_lists_page_without_loss`
- `cargo test --test catalog bucket_list_metadata_budget_preserves_large_rows_and_cursor_boundaries`

The bucket HTTP tests cover both native and legacy-emulator resource profiles,
create/get, ignored unknown rule fields, PATCH preservation/replacement/null and
empty clearing, PUT replacement/default clearing, restart, malformed input and
eight-way conditioned mutation. XML unit tests cover ordered matching, headers,
wildcards, max-age default/zero, credentials, malformed/duplicate input and
subdomain boundaries. Integrated strict-auth/browser/SDK evidence is recorded
with the final CORS verification gate.
