# JSON object listing slice

Contract checked on **2026-10-22**. The affected rows are JSON-OBJECTS-008,
BEHAVIOR-OBJECT-018 and the object-listing portions of BEHAVIOR-COMMON-009/013.
Implementation status belongs to [the ledger](../../../compatibility/features.json);
[execution evidence](../evidence/object-list.md) records the verified subset.

## Supported contract

`GET /storage/v1/b/{bucket}/o` returns live objects in a flat-namespace bucket.
The request has no body. Bucket path decoding, the 16 KiB raw-query limit, duplicate
decoded-key rejection and unknown-parameter errors follow the existing JSON parser.
No object body is opened or read. Metadata-only `POST` at the same path remains an
unimplemented objects.insert route and retains its previous 404 response; the
listing implementation does not advertise that valid Google method as
forbidden.
PUT/PATCH/DELETE on the collection return 405 `methodNotAllowed` with `Allow: GET`.

| Parameter | Supported behavior |
| --- | --- |
| `prefix` | Default empty; exact UTF-8 byte prefix; names remain relative to the bucket root. An object equal to the prefix remains eligible. |
| `delimiter` | Default empty; the first complete delimiter after `prefix` forms a common prefix, including the delimiter. Multi-character and multibyte delimiters are supported. |
| `startOffset` | Inclusive name bound; empty means no additional lower bound. Applied to object names before grouping. |
| `endOffset` | Exclusive name bound; empty means no additional upper bound. Equal or reversed effective bounds return an empty list. |
| `maxResults` | uint32; default 1000, zero selects that default, clamped to 1000 for both projections; counts items plus prefixes. Zero-as-default follows existing bucket listing and remains unverified against the real JSON service. |
| `pageToken` | Opaque local cursor; omitted/empty starts a listing. Validated against bucket, prefix, delimiter, offsets, trailing-delimiter setting and projection; page size may change. |
| `includeTrailingDelimiter` | Default false. True includes a marker ending in exactly one delimiter in `items` as well as its common prefix. Repeated terminal delimiters do not become marker items. |
| `projection` | `noAcl` default and `full` use the existing object renderer. Full omits unimplemented ACL/owner fields, exactly as existing metadata GET does. |
| `versions` | Omitted/false selects the sole live generation; true returns 501 `notImplemented`. No version-history claim. |
| `softDeleted`, `includeFoldersAsPrefixes` | Omitted/false supported; true returns 501. Invalid booleans return 400 `invalidParameter`. |
| `matchGlob` | Omitted/empty means no glob filter. The pinned Go SDK always sends the empty value. Every nonempty pattern returns 501, including patterns that happen to match all objects. |
| `filter` | Omitted/empty means no context filter; nonempty values return 501. |
| `fields`, `userProject` | Explicit 501 even for empty values; field selection and Requester Pays billing are unavailable. `userProject` follows existing bucket behavior. |
| `alt`, `prettyPrint` | Omitted/`alt=json` and omitted/`prettyPrint=false` supported; unknown alt returns 400 `invalidAltValue`; `alt=media` and `prettyPrint=true` return 501. |
| `key`, `oauth_token`, `quotaUser`, `userIp`, `callback` | Recognized and rejected with 501, matching the existing local API policy. |

Unknown or misplaced parameters, duplicate keys, malformed escapes/UTF-8, invalid
booleans/projections/uint32 values and invalid/mismatched tokens return 400
`invalidParameter`. A missing bucket returns 404 `notFound`, including when the
effective name interval is empty. A supplied body returns 400 `invalidArgument`.
Unsupported semantics use the existing emulator-specific 501 `notImplemented`,
which is not claimed to be Google's response for supported production features.

The response always contains `kind: "storage#objects"`. Nonempty `items` reuse
`object_resource`, so every rendered field and subsequent metadata extension is
shared with metadata GET. Nonempty `prefixes` are binary-sorted and deduplicated
over the whole traversal. Empty arrays are omitted. `nextPageToken` appears only
when an additional entry exists at the time the page is read; deletion can make a
later continuation empty. The final page has no token.

A trailing marker contributes two entries to the combined limit. With
`maxResults=1`, its item appears first and its prefix appears on the next page.
This avoids either exceeding the limit or losing one representation. Separate
arrays retain their own name ordering; no combined-array ordering is promised.

## Pagination, consistency and resources

Each result uses an indexed `(bucket, name)` seek with `LIMIT 1`, followed by at
most one lookahead seek. Common-prefix results advance the lower bound to the
binary successor of the complete prefix, skipping the entire subtree. Successor
bounds remain byte arrays cast to SQLite TEXT for BINARY comparison, including
when the successor itself is not valid UTF-8. No `LIKE`, `OFFSET`, catalog scan,
payload collection or schema change is needed. Per-page work is bounded by the
requested entry count times indexed-seek cost, independently of objects hidden
under a common prefix. Memory retains at most one page plus a lookahead record.

The existing catalog execution permit serializes a complete page with ordinary
mutations; no transaction spans requests. Pages see currently committed state,
not an authenticated snapshot. Deleting a cursor object does not invalidate the
next seek. Newly created names behind the cursor stay excluded; surviving names
ahead remain eligible. Returned common prefixes are not emitted again when new
children appear under them. Deleting a trailing marker between its item and
prefix pages still returns the prefix if surviving children justify it. Tokens
survive a normal restart and work without an in-memory token registry.

Tokens are versioned and use the existing bucket cursor's corruption checksum
for their payload and query fingerprint. They are not authorization credentials,
cryptographic signatures or Google-compatible token encodings. Malformed,
corrupted, wrong-version, wrong-shape and query-mismatched tokens are rejected;
deliberate reconstruction with a recomputed checksum is not prevented.

## Acceptance cases

| Case | Observable check | Executable evidence |
| --- | --- | --- |
| Route, resource and persistence | GET collection, empty omission, identical GET/list resources, default booleans/projection, full subset and restart | `object_list_resources_defaults_empty_and_restart` |
| Grouping and bounds | Empty prefix, prefix equal to an object, root-relative prefixes, duplicate groups, offsets inside a group, reversed bounds, UTF-8 binary bounds, multi-character/multibyte delimiter and single-entry pages | `object_list_prefix_delimiter_offsets_unicode_and_single_entry_pages` |
| Failures | Exact statuses/reasons for missing bucket, invalid inputs, unsupported options, wrong methods and a supplied body | `object_list_invalid_unsupported_and_missing_bucket_errors`, `bkt_http_query_body_name_and_route_boundaries` |
| Cursor validation | Corruption, all query dimensions, changed page size, empty token, invalid version/payload/checksum/shape/length | `object_list_tokens_bind_query_and_reject_corruption`, `object_list_tokens_reject_valid_checksums_with_invalid_payloads` |
| Concurrent changes between pages | Cursor and lookahead deletion, overwrite, insertion behind/ahead, restart, deleted marker with children and already-returned prefix | `object_list_mutations_between_pages_do_not_duplicate_or_skip_survivors` |
| Page limits | 1001 live objects: default, full and uint32 maximum clamp to 1000, then exactly one final object | `object_list_default_and_upper_page_limit_are_1000_for_both_projections` |
| Bounded SQL | A common prefix containing 20,000 objects is skipped within a 10,000 SQLite VM instruction budget, including continuation to the next item | `object_list_seeks_past_large_prefixes_with_bounded_sql_work` |
| Official SDK | Go Bucket.Objects, PageInfo.MaxSize=1 and iterator.NewPager size 1; prefix/delimiter/offsets/markers/UTF-8/long delimiter and default full projection | `TestObjectList` |
| Authorization and broader features | Pending: storage.objects.list/getIamPolicy enforcement, ACL projection, history, soft delete, managed folders, glob/context filters, billing and fields | Explicit rejection or inherited documented limitation; no passing permission claim |

Listing is read-only and does not evaluate generation preconditions or change
metadata, blobs, generations or metagenerations. Such misplaced query parameters
are rejected. Existing upload/delete tests remain the authority for atomic
mutations and recovery. Authentication remains unimplemented; production requires
`storage.objects.list`, and ACL projection additionally requires
`storage.objects.getIamPolicy`. Local success establishes neither permission.

## Sources and unresolved differences

The following official sources were checked on **2026-10-22**:

- [objects.list](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list)
- [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects)
- [JSON errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
- [Consistency](https://docs.cloud.google.com/storage/docs/consistency)
- [Discovery v1](https://storage.googleapis.com/$discovery/rest?version=v1), revision
  `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`;
  re-fetched bytes match the committed upstream source lock. Its maxResults
  description supplies the 1000 cap, default and combined item/prefix accounting.
- [Storage v2 proto](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto),
  latest file-changing commit `d73dfb3c3be52245625b951b9c0bcf65163c549f`, SHA-256
  `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.
  RPC defaults are supporting evidence, not substituted for JSON wire rules.
- [Pinned Go HTTP list implementation](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/http_client.go)
  and [Query](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/storage.go),
  storage v1.69.0: sends default empty matchGlob and full projection.
- [Generated JSON Objects response](https://github.com/googleapis/google-api-go-client/blob/v0.306.0/storage/v1/storage-gen.go),
  API v0.306.0: items/prefixes/nextPageToken use `omitempty`.

Existing gaps were reconciled: none mandates a different live-list implementation.
New listing gaps in `compatibility/gaps.json` keep the following limits visible:

- Discovery permits `maxResults=0`, but JSON sources do not define its continuation
  semantics. Zero selects the local default, matching existing bucket listing;
  exact production behavior remains unverified.
- The method guide includes slash-specific prefix wording while Discovery and the
  response table specify the requested delimiter. This slice follows the latter
  for arbitrary delimiters. No real-service differential test settles the wording.
- The source set does not explicitly settle empty-array wire omission, zero-size
  pages, duplicate marker accounting at size 1 or every token-binding dimension.
  Omission follows the pinned generated schema; marker paging and token validation
  are explicit local rules. No real-cloud request was made.
- Full projection, auth, unsupported advanced list semantics and checksum-only
  cursors keep the affected rows PARTIAL. Physical global consistency, snapshot
  listing, RSS/cgroup/CPU benchmarks, Windows and container verification are not
  established by these tests.

## Getting-started snippet

Start a source-built server with the ordinary local configuration, then set
`endpoint` to its loopback origin. The executed transcript uses an isolated
temporary catalog and records the exact response JSON in
[the guide log](../evidence/object-list-guide.txt).

```sh
endpoint=http://127.0.0.1:4443
curl --fail --silent --show-error -X POST -H 'Content-Type: application/json' \
  --data '{"name":"list-demo"}' "$endpoint/storage/v1/b?project=local-list" >/dev/null
for name in 'docs%2F' 'docs%2Fa.txt' 'docs%2Fnested%2Fb.txt'; do
  curl --fail --silent --show-error -X POST -H 'Content-Type: text/plain' \
    --data 'hello' \
    "$endpoint/upload/storage/v1/b/list-demo/o?uploadType=media&name=$name" >/dev/null
done
curl --fail --silent --show-error \
  "$endpoint/storage/v1/b/list-demo/o?prefix=docs%2F&delimiter=%2F"
curl --fail --silent --show-error \
  "$endpoint/storage/v1/b/list-demo/o?prefix=docs%2F&delimiter=%2F&maxResults=1"
```

The first listing contains items `docs/` and `docs/a.txt`, plus prefix
`docs/nested/`. The second contains only the marker item and a nextPageToken.
Continue by supplying that token with the same filters; page size may change.
For an empty matching prefix the exact local body is `{"kind":"storage#objects"}`.
