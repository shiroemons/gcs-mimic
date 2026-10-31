# JSON object metadata mutation

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked: **2026-10-22**. Scope: JSON-OBJECTS-010, JSON-OBJECTS-015,
BEHAVIOR-COMMON-006 and the metadata-related cases of BEHAVIOR-OBJECT-001,
BEHAVIOR-OBJECT-002, BEHAVIOR-OBJECT-006 and BEHAVIOR-COMMON-011.

## Acceptance cases

| Behavior | Expected result |
| --- | --- |
| PATCH/PUT encoded object path | 200 full Object resource; identical body/generation/checksums/creation time |
| PATCH fixed metadata | Only supplied fields change; null clears a supplied field |
| PATCH custom metadata | Merge keys; null removes one key; metadata:null clears the map |
| PUT writable metadata | Replace the set; omitted fields reset, content type defaults to application/octet-stream |
| Four generation/metageneration conditions | Matching comparisons succeed; failed comparisons return 412 conditionNotMet atomically |
| generation selector | Live generation succeeds; other generations return 404 notFound |
| Revision transition | Metageneration increments once; updated uses injected clock; ETag changes |
| Media headers | Stored content fields are reflected while bytes/checksums remain unchanged |
| Malformed JSON/types | 400 parseError/invalidArgument; state unchanged |
| Custom metadata size | Sum of UTF-8 key/value bytes <=8192; excess returns 400 invalidArgument atomically |
| Unsupported fields/query/headers | Explicit 501 notImplemented; no ignored recognized semantics |
| Missing bucket/object | 404 notFound |
| Conditioned concurrent updates | Exactly one 200; the other 412; one metageneration increment |
| Restart/schema migration | Persist metadata; migrate v3 objects with empty metadata and absent optional fields; retain v1 path |
| Authorization/projection | Existing unauthenticated local profile; full/noAcl expose modeled subset, no permission claim |
| Go SDK | Update, empty string value preservation, empty map deletion, conditioned success/412, Attrs and NewReader content type |

## Supported subset

`PATCH /storage/v1/b/{bucket}/o/{object}` merges the supplied mutable fields.
`PUT` on the same path replaces the entire supported writable set. Both operations
return 200 with the complete modeled Object resource. Paths preserve the existing
exactly-once percent decoding. Upload and download namespaces retain their existing
method boundaries. This slice does not change listing.

Supported fixed fields are `contentType`, `contentEncoding`, `contentDisposition`,
`contentLanguage` and `cacheControl`. Custom `metadata` preserves key spelling and
Unicode strings. Null clears a fixed field or the entire custom map; null map values
delete individual keys. Empty map PATCH is a no-op; PUT replaces the map. Empty custom
values are stored as empty strings. The final merged custom key/value UTF-8 size is
limited to 8192 bytes, including retained keys. The JSON body has the existing 64 KiB cap.
Invalid types, control characters in fixed fields and empty `contentLanguage` return
400 `invalidArgument`; malformed JSON returns 400 `parseError`. Oversized JSON returns
413 `uploadTooLarge`. `contentType:""` is a PATCH no-op; null removes it. When absent,
metadata JSON omits contentType and media serves `application/octet-stream`.
Empty disposition, encoding and cache-control values clear their fields.
Content-Language is limited to 100 characters (100 succeeds, 101 fails).
Non-ASCII fixed header values and empty custom metadata keys conservatively return
501 as unsupported semantics; custom values and nonempty keys retain Unicode.

`generation` selects only the live version; other values return 404 `notFound`.
All four generation/metageneration match and not-match parameters use the existing
signed-int64 parser and are evaluated with the mutation in one immediate SQLite
transaction. Failed comparisons return 412 `conditionNotMet`, never a read-style 304.
Missing buckets/objects return 404. Conditions precede server-managed input checks.
Successful calls, including empty updates, increment metageneration exactly once,
set `updated` from the injected clock, and change the opaque local ETag. Generation,
creation time, size, hashes, storage class and the blob reference stay unchanged.

Projection accepts `full` and `noAcl`; PUT defaults to full, PATCH to noAcl. Both expose
only modeled fields because authentication, permissions, ACLs and owner are not
implemented. `userProject`, `predefinedAcl`, `overrideUnlockedRetention`, partial fields,
encryption/conditional headers and method override remain explicit 501 rejections.
Recognized writable fields `acl`, holds, retention, contexts, customTime and encryption
settings are rejected even when null. Custom time is not stored; its monotonicity is
outside this slice. Unknown input fields also fail 501 pending legacy JSON evidence.

GET resources can be PUT back unchanged. Except for the scoped null-generation
case below, server-managed fields are accepted only when
their JSON values exactly equal the selected resource, checked inside the transaction:
kind, id, selfLink, mediaLink, name, bucket, generation, metageneration, size, md5Hash,
crc32c, etag, storageClass and creation/update/finalization/storage-class timestamps.
Decimal numeric JSON generation/metageneration/size values also match the equivalent
stored integer. Changing them, supplying the wrong type, or supplying an unmodeled output field returns
501 `notImplemented`. This conservative rule is a local compatibility boundary, not a
claim that Google rejects changed output-only input; see SPEC-015. In particular,
storageClass changes require rewrite and are not silently ignored.

### SDK null-generation exception

The integrated metadata mutation handler treats a body `generation: null` as
unspecified for PATCH, PUT and POST with PATCH override. Java Storage 2.76.0
`StorageImpl.update` selects generation even when absent and serializes
`Data.nullOf(Long.class)`. Actual JUL capture confirms that body and method
override. The pinned source archive and digest are recorded in SPEC-015.

This body field does not select a generation or act as a write/precondition:
query parameters retain those roles. Other read-only nulls, changed non-null
generation, unknown inputs, selected-generation 404, stale-condition 412 and
strict authorization denials remain enforced. Two dedicated wire tests verify
identity/hash/bytes preservation and these rejection boundaries; the unchanged
official Java multipart/PATCH/resumable/read/list scenario now passes against
the repaired runtime. This is a narrow SDK-backed local interpretation, with
actual Google service differential verification unexecuted.

The remaining sections record the original slice baseline. Later authentication,
ACL, hold, timestamp and download-transcoding additions have their own contracts
and evidence in the canonical feature ledger.

Media returns stored content type, encoding, disposition and language,
plus the existing lengths/hashes/generation/ETag headers. Custom metadata is returned
in Object JSON. The JSON common-header reference defines `x-goog-meta-*` as resumable
upload request headers; this slice does not add XML custom-metadata response headers.
Gzip media requiring decompressive transcoding fails 501. `Accept-Encoding` honors
gzip/wildcard tokens and valid q values; an explicit gzip quality overrides wildcard,
and q=0 refuses that coding. Invalid quality/duplicate gzip or wildcard tokens return
400 `invalidParameter`. Allowed gzip or stored `no-transform` permits unchanged bytes;
no automatic compression/decompression is performed. Double-compression detection is
outside the supported subset.

JSON media always returns `Cache-Control: no-cache`, as required for nonpublic objects.
Stored cache control is exposed in resource JSON and still determines the no-transform
transcoding rule. Public ACL/IAM grants are unavailable, so every local object uses the
nonpublic response. Public objects returning their stored cache directive remain for
the public/private access slice; metadata updates do not grant public access.

The general metadata guide says response headers are "based on the stored metadata
values". The JSON common-header reference explicitly says "sets the response to
`Cache-Control: no-cache`, regardless of the metadata settings" for nonpublic objects.
The latter is specific to this protocol and access classification, so it determines
this implementation. Both sources were checked on the date above; public behavior
remains outside the modeled permissions scope.

SDK invocation tokens are accepted without response replay, as an extension of the
existing GET/DELETE local rule. An unconditioned retry may increment metageneration
again; a stale metageneration condition fails 412. Do not claim token deduplication.

Schema v4 adds four nullable fixed metadata columns and an empty JSON custom map.
v3-to-v4 migration is transactional and does not touch blobs. Existing v1 migration
continues through the existing generation schema to v4; unreleased v2 remains rejected.
Existing objects retain content type and receive absent optional metadata/default empty
map. The blob publish/reclaim/recovery protocol is unchanged. Updates touch one catalog
row and never read the body or enumerate the object catalog.
Tests construct the original v3 schema directly and inject a late migration DDL failure
to verify that earlier column additions, user_version and object data roll back.
Conditioned races with delete and overwrite preserve one valid identity/body pair;
re-upload resets fixed/custom metadata for the new generation.

The fixed Go SDK sends `map[string]string{"key":""}` as a JSON empty string, not null.
It sends `map[string]string{}` as metadata:null to delete the map. Tests preserve this
distinction rather than changing the server's JSON contract to make an empty string
delete a key. Individual null deletion is verified by the HTTP integration tests.

All seven affected rows remain PARTIAL: the supported local metadata subset passes,
while permissions, unsupported properties, output-input parity and other transports
remain incomplete. [Evidence](../evidence/object-metadata.md) records commands/results.

## Sources

All URLs checked on 2026-10-22; the retained Discovery baseline is revision 20261013,
SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.

- [PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/patch)
- [PUT](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/update)
- [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects)
- [Patch semantics](https://docs.cloud.google.com/storage/docs/json_api#patch)
- [Metadata limit](https://docs.cloud.google.com/storage/quotas#objects)
- [Metadata](https://docs.cloud.google.com/storage/docs/metadata)
- [Gzip transcoding](https://docs.cloud.google.com/storage/docs/transcoding)
- [Output-only field guidance](https://google.aip.dev/203#output-only)
- [JSON headers and standard parameters](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters)
- [Pinned Go HTTP metadata conversion](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/http_client.go)
- [Pinned Go update attributes](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/storage.go)
- [Pinned schema](../../../compatibility/upstream/json-schemas.json)
- [Pinned methods](../../../compatibility/upstream/json-methods.json)

## Getting-started snippet

Run from the checkout after `cargo +1.99.1 build --locked --offline`; requires curl and
jq. The isolated server directory is deleted on shell exit. Commands below were
executed with Bash; the [recorded output](../evidence/object-metadata-guide.txt) shows
metagenerations 1, 2, 3, stale 412, then 4, with the original body unchanged.

```sh
set -euo pipefail
metadata_data=$(mktemp -d)
target/debug/gcs-mimic serve --listen 127.0.0.1:0 --data-dir "$metadata_data" \
  > "$metadata_data/server.log" 2>&1 &
metadata_pid=$!
trap 'kill -TERM "$metadata_pid"; wait "$metadata_pid"; rm -rf "$metadata_data"' EXIT
for attempt in {1..100}; do
  if jq -e 'select(.event == "ready")' "$metadata_data/server.log" > /dev/null 2>&1; then break; fi
  sleep 0.1
done
export GCS_METADATA_ENDPOINT="http://$(jq -r 'select(.event == "ready") | .listen' "$metadata_data/server.log")"

curl --fail --silent -X POST -H 'Content-Type: application/json' \
  --data '{"name":"metadata-example"}' \
  "$GCS_METADATA_ENDPOINT/storage/v1/b?project=local" | jq '{name}'
curl --fail --silent -X POST -H 'Content-Type: text/plain' \
  --data-binary 'hello metadata' \
  "$GCS_METADATA_ENDPOINT/upload/storage/v1/b/metadata-example/o?uploadType=media&name=hello.txt" \
  | jq '{name, metageneration, contentType}'
curl --fail --silent -X PATCH -H 'Content-Type: application/json' \
  --data '{"contentLanguage":"en","cacheControl":"private, max-age=60","metadata":{"team":"dev","remove":"old"}}' \
  "$GCS_METADATA_ENDPOINT/storage/v1/b/metadata-example/o/hello.txt?ifMetagenerationMatch=1" \
  | jq '{metageneration, contentType, contentLanguage, cacheControl, metadata}'
curl --fail --silent -X PATCH -H 'Content-Type: application/json' \
  --data '{"metadata":{"remove":null,"stage":"test"}}' \
  "$GCS_METADATA_ENDPOINT/storage/v1/b/metadata-example/o/hello.txt?ifMetagenerationMatch=2" \
  | jq '{metageneration, metadata}'
curl --silent -X PATCH -H 'Content-Type: application/json' \
  --data '{"metadata":{"stale":"no"}}' \
  "$GCS_METADATA_ENDPOINT/storage/v1/b/metadata-example/o/hello.txt?ifMetagenerationMatch=2" \
  | jq '{code:.error.code, reason:.error.errors[0].reason}'
curl --fail --silent -X PUT -H 'Content-Type: application/json' \
  --data '{"contentType":"application/x-example","metadata":{"only":"replacement"}}' \
  "$GCS_METADATA_ENDPOINT/storage/v1/b/metadata-example/o/hello.txt?ifMetagenerationMatch=3" \
  | jq '{metageneration, contentType, contentLanguage, cacheControl, metadata}'
curl --fail --silent -D "$metadata_data/headers" -o "$metadata_data/download" \
  "$GCS_METADATA_ENDPOINT/storage/v1/b/metadata-example/o/hello.txt?alt=media"
rg -i '^content-type:' "$metadata_data/headers"
printf 'Body: '
tr -d '\r' < "$metadata_data/download"
printf '\n'
```

Expected final metadata projection:

```json
{
  "metageneration": "4",
  "contentType": "application/x-example",
  "contentLanguage": null,
  "cacheControl": null,
  "metadata": {"only": "replacement"}
}
```

The null values above come from jq's projection of omitted fields. Download output is
`content-type: application/x-example` and `Body: hello metadata`.
