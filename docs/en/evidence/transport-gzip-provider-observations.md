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
