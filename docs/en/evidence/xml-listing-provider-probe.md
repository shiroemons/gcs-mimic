# XML listing and multipart answers recorded from Cloud Storage

<!-- markdownlint-configure-file {"MD013": false} -->

Recorded on **2026-11-16** (UTC). These observations are the source of the gap entries SPEC-XML-LIST-V2, SPEC-XML-MULTIPART-LIST and SPEC-XML-MULTIPART-CONTENT-TYPE in the [gap ledger](../../../compatibility/gaps.json). They cover only the requests listed below and do not establish XML API parity.

The floci-gcp 0.13.0 release notes (#558, #574, #576 and #602) described Cloud Storage answers for these requests that gcs-mimic does not give. floci-gcp is a comparison implementation, not the contract, so each answer was recorded from Cloud Storage before being recorded here.

## How it was recorded

- An explicitly authorized project, `<project>`, with an access token from the owner's gcloud credentials, held only in a shell variable and never printed.
- One disposable bucket named with the harness prefix `gcsmimic-diff-` and the label `gcs-mimic-differential`, so a differential sweep would recognize a leftover: us-central1, STANDARD, public access prevention enforced, soft delete off.
- Three 1-byte objects (`a`, `b`, `d/c`) written over the XML API, and two multipart uploads (`d/mp.bin`, `e.bin`) started without parts.
- 24 requests to Cloud Storage. Both multipart uploads were aborted (204), the listing then held no upload, the three objects and the bucket were deleted (204 each), and a bucket GET answered 404 afterwards.
- The requests were sent with curl, not the [differential harness](differential-harness.md): a harness capture reads only JSON bodies and headers, so a case cannot read the `UploadId` it needs to abort the uploads. These answers are therefore not part of the replayed [reference](../../../compatibility/differential/reference/gcs.json).
- The same requests were sent to gcs-mimic 0.24.0 (`--scheme http`, default settings) for the comparison column.

Bucket names, upload IDs and generations are replaced with placeholders below. Each `Contents` entry is shown once.

## Object listing, `list-type=2`

| Request | Cloud Storage | gcs-mimic 0.24.0 |
| --- | --- | --- |
| `GET /BUCKET?list-type=2` | 200, `Name`, `Prefix`, `KeyCount`, `IsTruncated`, `Contents`; no `MaxKeys`; no `StorageClass` in `Contents` | 200, `Name`, `Prefix`, `MaxKeys`, `IsTruncated`, `KeyCount`, `Contents` with `StorageClass` |
| `…&continuation-token=` (empty) | 200, `<ContinuationToken></ContinuationToken>` after `Prefix`; listed from the start | 400 `InvalidArgument`, `Invalid XML continuation-token` |
| `…&start-after=` (empty) | 200, `<StartAfter></StartAfter>` after `Prefix` | 200, `<StartAfter></StartAfter>` after `KeyCount` |
| `…&continuation-token=&start-after=a` | 200, `<StartAfter>a</StartAfter><ContinuationToken></ContinuationToken>` after `Prefix`; `KeyCount` 2 (`b`, `d/c`) | 400 `InvalidArgument` |

Cloud Storage, `continuation-token=&start-after=a`:

```xml
<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name><bucket></Name><Prefix></Prefix><StartAfter>a</StartAfter><ContinuationToken></ContinuationToken><KeyCount>2</KeyCount><IsTruncated>false</IsTruncated><Contents><Key>b</Key><Generation><generation></Generation><MetaGeneration>1</MetaGeneration><LastModified>2026-11-16T06:37:50.965Z</LastModified><ETag>"9dd4e461268c8034f5c8564e155c67a6"</ETag><Size>1</Size></Contents>…</ListBucketResult>
```

gcs-mimic 0.24.0, `list-type=2`:

```xml
<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name><bucket></Name><Prefix></Prefix><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><KeyCount>3</KeyCount><Contents><Key>a</Key><Generation><generation></Generation><MetaGeneration>1</MetaGeneration><LastModified>2026-11-16T06:37:38.529Z</LastModified><ETag>"9dd4e461268c8034f5c8564e155c67a6"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Contents>…</ListBucketResult>
```

Not recorded: a truncated version 2 page (the position of `NextContinuationToken`), `delimiter` and `CommonPrefixes`, `encoding-type`, `fetch-owner`, `max-keys` in version 2 and a non-empty invalid token. The version 1 listing (`xml-list-bucket`, recorded 2026-11-01) still matches.

## Multipart initiation and upload listing

Every Cloud Storage answer below was 200 with an XML document body. Cloud Storage sent `Content-Type: text/html; charset=UTF-8` on both the initiation (`POST /BUCKET/KEY?uploads` with `Content-Length: 0`) and every listing (`GET /BUCKET?uploads`); gcs-mimic sends `application/xml; charset=UTF-8`. The initiation bodies are the same apart from the bucket and upload ID.

| Listing request | Cloud Storage elements before the first `Upload` | gcs-mimic 0.24.0 |
| --- | --- | --- |
| no parameters | `Bucket`, `KeyMarker`, `UploadIdMarker`, `NextKeyMarker` (empty), `NextUploadIdMarker` (empty), `MaxUploads` 1000, `IsTruncated` false | `Bucket`, `KeyMarker`, `UploadIdMarker`, `Prefix` (empty), `Delimiter` (empty), `MaxUploads`, `IsTruncated`; no `Next*` |
| `delimiter=/` | … `NextKeyMarker`, `Delimiter`, `NextUploadIdMarker`, `MaxUploads` …; `e.bin` and `CommonPrefixes` `d/` | same entries; `Prefix`, `Delimiter` before `MaxUploads` |
| `prefix=d/` | … `NextKeyMarker`, `Prefix`, `NextUploadIdMarker`, `MaxUploads` …; `d/mp.bin` | same entry; `Prefix`, `Delimiter` before `MaxUploads` |
| `encoding-type=url` | 200, both uploads, then `<EncodingType>url</EncodingType>` after the last `Upload` | 400 `InvalidArgument`, encoding-type unsupported |
| `max-uploads=5000` | `MaxUploads` 1000 | `MaxUploads` 1000 |
| `max-uploads=1` | `NextKeyMarker` `d/mp.bin` and `NextUploadIdMarker` before `MaxUploads` 1, `IsTruncated` true | `NextKeyMarker` and `NextUploadIdMarker` after `IsTruncated` |

`Initiated` carried microseconds on Cloud Storage (`2026-11-16T06:37:53.548719Z`) and milliseconds on gcs-mimic (`2026-11-16T06:37:38.770Z`).

Cloud Storage, `max-uploads=1`:

```xml
<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket><bucket></Bucket><KeyMarker></KeyMarker><UploadIdMarker></UploadIdMarker><NextKeyMarker>d/mp.bin</NextKeyMarker><NextUploadIdMarker><upload-id></NextUploadIdMarker><MaxUploads>1</MaxUploads><IsTruncated>true</IsTruncated><Upload><Key>d/mp.bin</Key><UploadId><upload-id></UploadId><StorageClass>STANDARD</StorageClass><Initiated>2026-11-16T06:37:53.548719Z</Initiated></Upload></ListMultipartUploadsResult>
```

gcs-mimic 0.24.0, `max-uploads=1`:

```xml
<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket><bucket></Bucket><KeyMarker></KeyMarker><UploadIdMarker></UploadIdMarker><Prefix></Prefix><Delimiter></Delimiter><MaxUploads>1</MaxUploads><IsTruncated>true</IsTruncated><NextKeyMarker>d/mp.bin</NextKeyMarker><NextUploadIdMarker><upload-id></NextUploadIdMarker><Upload><Key>d/mp.bin</Key><UploadId><upload-id></UploadId><StorageClass>STANDARD</StorageClass><Initiated>2026-11-16T06:37:38.770Z</Initiated></Upload></ListMultipartUploadsResult>
```

Not recorded: `prefix` and `delimiter` together, which fields `encoding-type=url` encodes, `key-marker` and `upload-id-marker` paging, and the content types of part upload, list parts, completion and abort.

## Recorded again with the differential harness

On the same day the [differential harness](differential-harness.md) recorded two cases against Cloud Storage in the same authorized project, after it learned to capture a value from an XML body (`xml:ELEMENT`) and to abort open multipart uploads in cleanup:

- `xml-list-v2`: 18 steps on a bucket of its own (five objects `a`, `b`, `d/c`, `d/e`, `f g`).
- `xml-multipart-list`: 24 steps on a bucket of its own (four uploads, one completed with one part, three aborted).

The run sent 65 requests with no 429 retry. Both buckets were emptied, deleted and confirmed 404 on Cloud Storage and on gcs-mimic, and the final sweep found no leftover bucket. The recording was merged into the [reference](../../../compatibility/differential/reference/gcs.json) with `--add-new`; the other 122 cases did not change. The harness sends `uploads=` where curl sent `uploads`, and Cloud Storage answered both the same way. Every curl observation above was answered the same way again. The cases also recorded what curl had not:

- Version 2 listing: `NextContinuationToken` follows the echoes on a truncated page and precedes `KeyCount`; `MaxKeys` follows `KeyCount` only when `max-keys` is sent; `Delimiter` follows them; `EncodingType` follows `IsTruncated`. `KeyCount` counts the `Contents` entries only (3 for `a`, `b`, `f g` and the common prefix `d/`). `encoding-type=url` wrote `f g` as `f+g` and kept `d/c`. `fetch-owner=false` listed as usual. `continuation-token=bogus` answered 400 `InvalidArgument` with the message `Invalid argument.` and the details `Invalid page token.`.
- Upload listing: with both `prefix` and `delimiter`, `Prefix` precedes `Delimiter` between the next markers. `encoding-type=url` wrote `f g.bin` as `f+g.bin`, kept `d/` and put `EncodingType` after the common prefixes; `encoding-type=bogus` answered 400 `InvalidArgument` with the details `Invalid encoding type: bogus`, as `application/xml`. `key-marker` with `upload-id-marker` resumed after the named upload and echoed both; `key-marker` alone resumed after every upload of that name; `upload-id-marker` alone was ignored and echoed empty. Both listings sent `Cache-Control: private, max-age=0` with `Expires`.
- Part upload, part listing, completion and abort: the part upload's empty 200, the part listing and the completion were `text/html; charset=UTF-8`; the abort's 204 had no `Content-Type`. The part listing wrote `NextPartNumberMarker` 0 after `PartNumberMarker` on an untruncated page, and part `LastModified` with microseconds. Initiation and completion varied by `Origin`. Completion sent `x-goog-stored-content-length` and no `ETag` header, and wrote its `Location` with `http` for an `https` request.

gcs-mimic now answers both cases as recorded (SPEC-XML-LIST-V2, SPEC-XML-MULTIPART-LIST and SPEC-XML-MULTIPART-CONTENT-TYPE are resolved), and the offline replay of every case reports no difference.

## Limits

- One run against one project and region. The comparison column above describes gcs-mimic 0.24.0, before the fixes.
- Characters other than a space and `/` were not recorded under `encoding-type=url`, nor a truncated part listing, nor an XML POST other than multipart initiation and completion; the ledger names the local choices for each.
- The Python SDK's `sdk.xml_multipart_upload` scorecard case, which initiates, uploads parts and completes an XML multipart upload, passed on gcs-mimic 0.24.0 with `application/xml`.
