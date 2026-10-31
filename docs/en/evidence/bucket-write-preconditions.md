# Cloud Storage preconditions on bucket writes and IAM policy writes

Observed on **2026-10-31** with curl against `storage.googleapis.com` (JSON API),
from an explicitly opted-in test project, using private, uniquely named buckets
that were deleted afterwards. Project, bucket names and principals are omitted.
This table is the provider evidence for docs/en/slices/json-bucket-write-etags.md.

| Request | Status |
| --- | --- |
| bucket PATCH `If-Match: <current etag>` | 200 |
| bucket PATCH `If-Match: <stale etag>` | 200; the header is ignored and the change applies |
| bucket PATCH `If-None-Match: <stale etag>` | 200 |
| bucket PUT (update) `If-Match: <current etag>` | 200 |
| bucket PUT (update) `If-Match: <stale etag>` | 200; the header is ignored and the change applies |
| setIamPolicy `If-Match: <current policy etag>` | 200 |
| setIamPolicy `If-Match: <stale policy etag>`, sent bare (unquoted base64) | 412 |
| setIamPolicy, stale `etag` in the body, no header | 412 |

Bucket etags were short opaque base64 values that advanced with metageneration.
The policy etag is the unquoted base64 value of the policy resource's `etag` field.

## Limits

One run, one location and one principal. Error bodies, the order of the header and
body checks when both are stale, weak or list-valued tags, `If-None-Match` on
setIamPolicy and malformed header syntax were not observed; the emulator's local
choices for those are listed in the slice contract.
