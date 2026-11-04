# XML request framing observed on Cloud Storage

<!-- markdownlint-configure-file {"MD013": false} -->

Observed **2026-11-04** by the project owner's coordinator using authenticated
curl requests to the `storage.googleapis.com` XML API over HTTP/1.1 and HTTP/2.
Every request targeted a non-existent bucket. No resources were created and no
charges were incurred. Paths below use placeholders; credentials and resource
identifiers are omitted. This is the supplied status-code summary, not a raw
exchange; the curl version was not recorded here.

## Observed responses

All requests had an empty body and no Transfer-Encoding. The Content-Length
header was either omitted or explicitly set to zero as shown.

| Request | HTTP/1.1 | HTTP/2 |
| --- | --- | --- |
| `POST /{bucket}/{object}?uploads`, no Content-Length | 411 from the front end, with an HTTP/1.0 response | 404 |
| `POST /{bucket}/{object}?uploads`, `Content-Length: 0` | 404 | 404 |
| `GET /{bucket}?uploads`, no Content-Length | 404 | 404 |
| `GET /{bucket}/{object}?uploadId=...`, no Content-Length | 404 | 404 |
| `DELETE /{bucket}/{object}?uploadId=...`, no Content-Length | 404 | 404 |
| `PUT /{bucket}/{object}` with `x-goog-copy-source`, no Content-Length | 411 from the front end, with an HTTP/1.0 response | 404 |
| `PUT /{bucket}/{object}` with `x-goog-copy-source`, `Content-Length: 0` | 404 | 404 |

Each 404 was `NoSuchBucket`: the request passed framing checks and reached
bucket lookup. It does not establish a successful upload, copy, listing or
abort against an existing bucket.

## Local contract and known difference

The emulator's HTTP API serves only HTTP/1.1: [TLS ALPN](../../../src/tls.rs)
advertises `http/1.1`, and the [HTTP server](../../../src/server.rs) uses Hyper's
HTTP/1 builder. The separate gRPC listener does not serve these XML routes.
The HTTP/1.1 column therefore backs the framing choices in
[`src/xml/put.rs`](../../../src/xml/put.rs): `Framing::Required` for multipart
initiation and object copy, and `Framing::OptionalEmpty` for list uploads,
list parts and abort. Initiation POST and copy PUT require Content-Length in
these non-chunked requests; bodyless GET and DELETE accept omitted framing.
The HTTP/2 results do not change the HTTP/1.1 contract.

Known local difference: Google's front-end 411 response has an HTML body and
uses HTTP/1.0, while the emulator returns an HTTP/1.1 XML `MissingContentLength`
error. Chunked transfer, malformed framing, nonempty bodies and successful
resource operations were not probed. Their existing local acceptance tests in
the [multipart slice](../slices/xml-multipart-upload.md) and
[copy slice](../slices/xml-object-copy.md) remain separate evidence.
