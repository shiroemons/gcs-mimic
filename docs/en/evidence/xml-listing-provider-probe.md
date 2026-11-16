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

## Limits

- One run against one project and region, sent by curl rather than replayed by the harness. A change to gcs-mimic needs a recorded and replayed case first, as SPEC-XML-MULTIPART-LIST describes.
- The Python SDK's `sdk.xml_multipart_upload` scorecard case, which initiates, uploads parts and completes an XML multipart upload, passes on gcs-mimic 0.24.0 with `application/xml`, so the content type has not broken that client.
