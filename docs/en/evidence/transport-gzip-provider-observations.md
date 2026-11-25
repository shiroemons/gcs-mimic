# Transport gzip and upload sessions observed on Cloud Storage

<!-- markdownlint-configure-file {"MD024": {"siblings_only": true}} -->

Observed **2026-11-25** with curl requests sent to `storage.googleapis.com`
from an opt-in test project, using private, uniquely named buckets that were
deleted afterwards. The project, buckets and principals are not recorded.
Anonymous requests carried no `Authorization` header; authenticated requests
used an owner's access token.

Bodies were built with `gzip -c`, then altered byte by byte: "not gzip" is
plain text, "truncated" drops the 8-byte trailer, "CRC zeroed" and "ISIZE
zeroed" replace that trailer field with zeros, and "trailing garbage" appends
`garbage` after a complete member.

## The front end decodes before it routes or authenticates

Every request below sent `Content-Encoding: gzip`.

| Request | Caller | Body | Status | Response |
| --- | --- | --- | --- | --- |
| Object PATCH | anonymous | valid | 401 | JSON error naming `storage.objects.update` |
| Object PATCH | anonymous | not gzip | 400 | `text/plain; charset=utf-8`, `Could not decompress request body.` |
| Object PATCH | anonymous | truncated | 400 | the same text |
| Object PATCH | anonymous | CRC zeroed | 401 | the same JSON error as the valid body |
| Object PATCH | anonymous | empty | 401 | the same JSON error as the valid body |
| Object PATCH | owner | not gzip | 400 | the same text |
| Object PATCH | owner | trailing garbage | 400 | the same text |
| Object PATCH | owner | CRC zeroed, then ISIZE zeroed | 200 | the metadata each body carries is applied |
| Object GET, no body | anonymous | none | 401 | JSON error naming `storage.objects.get` |
| Bucket insert | anonymous | not gzip | 400 | the same text |
| Resumable initiation | anonymous | not gzip | 400 | the same text |
| Media upload | anonymous | not gzip | 400 | the same text |
| Media upload | anonymous | truncated | 400 | the same text |
| Media upload | owner | CRC zeroed | 200 | `size` 5; the object reads back as the decoded `hello` |
| Multipart upload | anonymous | not gzip | 400 | the same text |
| Multipart upload | anonymous | truncated | 400 | the same text |
| POST `/storage/v1/nonexistent/route` | anonymous | not gzip | 400 | the same text |
| POST `/storage/v1/nonexistent/route` | anonymous | valid | 404 | `text/html; charset=UTF-8`, `Not Found` |

With `Origin: https://app.example` on the anonymous object PATCH, the 400 text
answers and the 401 errors all carried `access-control-allow-origin`.

A body that does not decode therefore answers 400 whoever sends it and
whatever route it names, and a body that decodes meets the same checks as the
same body sent plain. Every member needs its complete trailer, and nothing may
follow the last member, but the trailer's CRC-32 and size are not compared with
the decoded data. An empty body is not decoded.

## Upload session detection

| Request | Body | Status | Response |
| --- | --- | --- | --- |
| Media upload with `ifGenerationMatch=abc` | plain, then gzip | 400 | `Invalid long value: 'abc'.` both times |
| Resumable initiation whose query adds `upload%5Fid=zzz` | plain, then gzip | 404 | `Not Found` both times |

The front end decodes the query key `upload%5Fid` to `upload_id`, so the
initiation is a request on the session `zzz`, which does not exist.

## XML multipart abort with a part in flight

A part PUT sent its declared body slowly; the upload was aborted while the
part was still on the wire.

| Request | Status | Notes |
| --- | --- | --- |
| Abort (`DELETE ?uploadId=`) | 204 | answered after about one second, before the part finished |
| The part in flight, once its body was sent | 404 | `NoSuchUpload`, `The requested upload was not found.` |
| A part sent after the abort, and the parts listing | 404 | `NoSuchUpload` |
| The object | 404 | never created |

## gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| A body that does not decode answers 400 before routing and authentication | `ApiError::undecodable_body`: the same status, content type and text, with the CORS headers of a CORS route | `server::ingress::tests::a_gzip_body_is_expanded_before_the_caller_is_checked`, `gzip_and_plain_bodies_meet_the_same_refusal`, `gzip_expands_before_authentication_and_override_uses_patch_permissions`, `gzip_initiation_expands_before_strict_authentication_and_upload_permissions` |
| A body that decodes meets the same refusal as the plain body | JSON bodies and initiation metadata are expanded before dispatch | `gzip_and_plain_bodies_meet_the_same_refusal` |
| The trailer's CRC-32 and size are not compared | `server::ingress::members` reads each member's trailer and ignores its fields | `server::ingress::members::tests::a_trailer_that_disagrees_with_the_data_still_decodes`, `gzip_bodies_that_do_not_decode_never_mutate_metadata`, `media_transport_gzip_stores_the_decoded_body`, `multipart_gzip_checks_truncation_and_trailers_after_the_mime_close_but_not_checksums`, `invalid_gzip_initiations_never_create_sessions` |
| A truncated member or bytes after the last member fail the body | Refused as not decodable | `server::ingress::members::tests::a_body_that_ends_inside_a_member_or_continues_past_it_fails` and the HTTP tests above |
| An empty body is not decoded | Dispatched as an empty body without a decoder | `server::ingress::tests::a_gzip_body_is_expanded_before_the_caller_is_checked`, `media_transport_gzip_stores_the_decoded_body` |
| `upload%5Fid` names a session | `json::resumable::session_route` decodes query keys | `json::resumable::an_escaped_upload_id_key_names_the_session`, `gzip_and_plain_bodies_meet_the_same_refusal` |
| `NoSuchUpload` wording | `The requested upload was not found.` | `multipart_bodyless_operations_accept_omitted_framing` |

### Known differences

- A media or multipart upload streams its body to its handler, so only its
  first bytes are checked before dispatch. A caller that the handler refuses
  before it reads the body gets that refusal (401 or 403) when the body starts
  a gzip member but is cut short or followed by other bytes later on, where
  Cloud Storage answered the anonymous truncated media and multipart uploads
  with 400. Reading an upload of up to `max_object_bytes` from a refused caller
  only to report its encoding is not worth the unbounded wait.
- JSON bodies and initiation metadata keep the local 16 MiB limit on both the
  compressed and the expanded body, answered with 413. Cloud Storage's own
  limit was not probed.
- A gzip body on a resumable session request keeps its existing refusal; that
  combination was not probed.
