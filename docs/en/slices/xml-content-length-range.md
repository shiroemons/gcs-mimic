# XML upload size ranges

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-11-04**. Features: EXT-XML-022 and DESIGN-084. The
feature ledger remains authoritative; EXT-XML-022 stays PARTIAL for its other
advanced inputs. This slice adds `x-goog-content-length-range` to XML object
PUT so Rails Active Storage 8.1.4.1 direct uploads work.

## Why

Rails 8.1.4.1 is a security release. Two of its changes alter the GCS
direct-upload contract
([release](https://github.com/rails/rails/releases/tag/v8.1.4.1),
[gcs_service.rb at v8.1.4.1](https://github.com/rails/rails/blob/v8.1.4.1/activestorage/lib/active_storage/service/gcs_service.rb),
SHA-256 `e3252498d7c5c288628b8a923023ecef8cd93d588c93ba740664fd20f411a12d`):

- GHSA-hfr4-8rjc-97m2: `url_for_direct_upload` and `headers_for_direct_upload`
  require `content_length:` and sign and send
  `x-goog-content-length-range: <size>,<size>`, so a URL issued for one blob
  cannot store a body of another size.
- GHSA-3w93-6hmw-whh4: direct uploads are always signed with V2. A configured
  `cache_control` is sent as an unsigned `Cache-Control` header.

gcs-mimic 0.9.0 rejects every unrecognized `x-goog-*` header on XML PUT, so it
answered these uploads with `400 InvalidArgument`. The Rails guide at v8.1.4.1
also adds `x-goog-content-length-range` to the GCS CORS `responseHeader` list.

## Contract

The [XML header reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers)
defines the header as a `MIN,MAX` pair of inclusive byte counts on `PUT`
requests, returns `400 Bad Request` for a body outside it and ignores it on other
methods. It names no error codes or value grammar. Those come from Cloud Storage
responses recorded on 2026-11-04 by the `xml-content-length-range`
differential case (`compatibility/differential/cases/xml-content-length-range.json`,
reference in `compatibility/differential/reference/gcs.json`). The test bucket
was disposable and deleted after the run.

| Request | Cloud Storage | gcs-mimic |
| --- | --- | --- |
| Body inside the range, bounds inclusive (`5,5`, `5,10`, `0,5`, empty body with `0,0`) | 200 | 200 |
| Body larger than MAX | 400 `EntityTooLarge`, Details `Content-length exceeds upper bound on range`; no object, an existing object is kept | same |
| Body smaller than MIN | 400 `EntityTooSmall`, Details `Content-length smaller than lower bound on range`; no object | same |
| Not two comma-separated parts (`5`, `1,2,3`) | 400 `ContentLengthRangeInHeaderIsInvalid`, Message `Invalid argument.` | same |
| An empty bound (`1,`, `,5`, `,`) | 400 `ContentLengthRangeInHeaderIsInvalid`, Details `min and max range values cannot be null` | same |
| A bound that is not a signed 64-bit decimal (`a,b`, `1, 10`, `0,9223372036854775808`) | 400 `ContentLengthRangeInHeaderIsInvalid`, Details `x-goog-content-length-range: '<value>' is invalid` | same |
| A negative bound (`-1,5`, `5,-1`) | 400 `ContentLengthRangeInHeaderIsInvalid`, Details `range values cannot be negative` | same |
| MIN greater than MAX (`6,5`) | 400 `ContentLengthRangeInHeaderIsInvalid`, Details `x-goog-content-length-range: '6,5' is invalid` | same |
| `+1,5`, `-0,5`, `01,05`, `0,9223372036854775807` | 200 | 200 |
| The header on GET | ignored | ignored |
| The header on a copy (`x-goog-copy-source`) | ignored; the copy succeeds even when the source exceeds the range | ignored |

The bounds are parsed as signed 64-bit decimals, as `i64::from_str` does, so
whitespace is rejected and a sign is accepted. The checks run in the order of
the table: part count, empty bound, parse, sign, order.

## Implementation

- `src/xml/put/length_range.rs` parses the header with `TryFrom<&str>` into an
  inclusive range. The error type carries the recorded reason and converts
  into the XML error through `From`.
- A size declared by `Content-Length` or `x-amz-decoded-content-length` is
  checked while the headers are validated, before any body byte is read or
  staged.
- A chunked body declares no size. Its reader stops at the first byte past
  the smaller of MAX and the configured object limit and checks MIN at EOF.
  `stream_upload` reports whether the body ended or the catalog stopped it, so
  a body cut short by a storage failure is never measured as complete.
  Either failure ends the upload before EOF is committed, so nothing is
  published.
- V2 signatures include every `x-goog-*` header and V4 signatures require
  every `x-goog-*` header to be signed, so the signature binds the range in
  both versions without new signing code.
- XML multipart uploads keep rejecting the header as unsupported, because
  their parts and completions have not been compared with Cloud Storage. A
  silent accept would drop the size limit.

## Verification

Run on 2026-11-04 with Rust 1.99.1, Ruby 4.0.9 and Bundler 4.0.23:

- `cargo test --lib xml::put`: 10 passed, including the parser cases above.
- `cargo test --test it_xml_auth length_range`: 3 passed. A strict V4 signed
  URL accepts inclusive sizes, rejects larger and smaller bodies with
  Content-Length and chunked framing (including a 256 KiB chunked body) without
  replacing or creating the object, and returns `SignatureDoesNotMatch` when
  the range is removed or changed.
- `cargo xtask differential run --reference compatibility/differential/reference/gcs.json --filter xml-content-length-range`:
  match, 0 differing fields over 26 steps.
- `just check-active-storage` with activestorage and railties 8.1.4.1:
  scenario `1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL` (the known
  AS-DEFAULT-AUTH failure) and storage.yml recipe `11 PASS, 0 FAIL`.
  AS-EMULATOR-DIRECT-UPLOAD-SIZE sends the stock signed URL a larger and a
  smaller body with matching Content-MD5 and gets `EntityTooLarge` and
  `EntityTooSmall` with no object. AS-EMULATOR-CACHE-CONTROL-DIRECT-UPLOAD
  verifies the V2 URL, the unsigned Cache-Control header, its CORS preflight
  and the stored Cache-Control.
- `bash sdk-tests/ruby/active_storage/run-strict.sh`: signing self-check PASS
  (the explicit `EmulatorGcs` V4 adapter signs the range, and a changed range
  breaks the signature) and `3 PASS, 0 FAIL`.

### Browser check

A one-file Rails 8.1.4.1 app outside the repository (Puma 8.0.2, the
documented permissive `gcs_emulator` entry and a `buckets.json` CORS rule
allowing `x-goog-content-length-range`) was checked in Chrome on 2026-11-04
against an emulator built from this change. Ports 13000 and 14443 were used
because 3000 and 4443 were taken. The page loaded the gem's
`activestorage.esm.js` and called `ActiveStorage.start()`; the PNG was created
in the page and assigned to the file input, because the automation cannot
drive the native file picker. The Upload button was clicked normally.

| Request from Chrome | Result |
| --- | --- |
| `POST /rails/active_storage/direct_uploads` | 200; a V2 URL with `x-goog-content-length-range: N,N` in the headers |
| CORS preflight to the emulator | 200 |
| Signed `PUT` to the emulator | 200; the blob was attached and the image loaded through the redirect |
| The same URL signed for 5 bytes, a 6-byte body | 400 `EntityTooLarge`, readable by the page through CORS |
| The same URL, a 4-byte body | 400 `EntityTooSmall` |
| The same URL, a 5-byte body | 200 |

## Limitations

- XML multipart part uploads, initiation and completion reject the header
  instead of following Cloud Storage, which has not been recorded for them.
- XML resumable initiation keeps rejecting the header.
- The browser check used the permissive profile only. Strict-mode browser
  uploads with Rails 8.1.4.1 remain unverified.
