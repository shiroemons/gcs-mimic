# HTTP connection semantics verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified **2026-10-28** for AUDIT-010 at code commit `a51400d` on `main`,
which keeps HTTP/1.1 connections alive on top of the deadline responses and
lingering close from `89d9e9a` and the TLS trickle test fix from `c794ad8`.

| Command | Result | Evidence |
| --- | --- | --- |
| `env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/check.sh` | PASS, 45 test binaries and doctests, compatibility check, fmt and clippy including cross targets | Local macOS ARM64 run |
| `env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh` | PASS, pinned Go SDK suite over HTTP with pooled connections in 3.8 s | Local macOS ARM64 run |
| GitHub Actions Foundation checks run 40643077472 | PASS, all eight jobs: pinned and stable Rust on Linux, native tests on macOS ARM64, Windows x86-64 and Linux musl, both container platforms and the Go SDK | <https://github.com/shiroemons/gcs-mimic/actions/runs/40643077472> |
| Server deadline and close changes reverted to their parent commit, updated tests run with `--no-fail-fast` | FAIL as intended for the six deadline and lingering tests listed below | Local negative check; source restored afterward |
| `.keep_alive(false)` restored temporarily, keep-alive test run | FAIL as intended: the second request on the connection reads EOF | Local negative check; source restored afterward |

## Verified scope

- A connection or absolute deadline that expires before the handler answers
  cancels the handler without publication and answers instead of closing
  silently. JSON object uploads receive 408 `uploadBrokenConnection`
  (`obj_http_trickle_upload_deadline_releases_permits_without_publication`,
  `obj_http_active_upload_outlives_total_deadline_then_idle_body_times_out`,
  `obj_multipart_http_deadline_cancels_admitted_bodies_and_releases_all_slots`,
  `tls_streaming_progress_and_upload_deadlines_preserve_existing_object`), and
  XML PUT receives the XML error code `uploadBrokenConnection`
  (`xml_put_chunked_trailers_and_body_deadlines_cannot_publish`).
- A bucket write held behind an external SQLite lock past the connection
  deadline receives 503 `backendError` with `Connection: close`, and the
  server stays usable (`bkt_http_deadline_before_response_returns_backend_error`).
- Lingering close lets a client finish writing an 8 MiB body after an early
  413 or 400 and still read that response on every CI platform
  (`obj_http_early_rejections_survive_body_larger_than_socket_buffers`). With
  the previous 256 KiB/100 ms close, the same write failed.
- A JSON upload rejected before its body is polled answers with the final
  response and no `100 Continue`
  (`obj_http_expect_continue_rejection_skips_the_interim_response`).
- A media download whose absolute deadline expires after its headers is still
  truncated (`obj_http_trickle_download_stops_at_absolute_deadline`).
- One connection serves bucket creation, a media upload and its download in
  sequence, each response with Content-Length and without `Connection: close`;
  the connection closes after the connection timeout when left idle, and an
  upload rejected with its body unread closes instead of parsing that body as
  the next request (`keep_alive_serves_sequential_requests_until_idle_timeout`).

## Sources

- [JSON API status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes):
  408 `uploadBrokenConnection` "The request timed out."; 503 `backendError`.
- [XML API status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status):
  408 `uploadBrokenConnection`.
- [Retry strategy](https://docs.cloud.google.com/storage/docs/retry-strategy):
  408, 429 and 5xx responses are retryable.
- [nginx lingering close](https://nginx.org/en/docs/http/ngx_http_core_module.html#lingering_close):
  default `lingering_time` 30 s and `lingering_timeout` 5 s.

## Limits

- Which provider status applies to each timeout is taken from the documented
  status/reason pairs; no real-cloud differential run confirms when Cloud
  Storage returns them.
- A request that carries a body to a non-upload route receives 503, because
  the handler is cancelled without knowing whether its small body was read.
- The emulator waits at most one second of client silence, shorter than
  nginx's 5 s `lingering_timeout`, so an idle client releases its slot sooner.
  A client that keeps sending can hold its connection slot for up to 30 s.
- `connection_timeout_ms` is both the header read deadline and the idle
  timeout between requests. A pooled client that reuses a connection at the
  moment it closes sees a closed connection; an idle connection also holds a
  `max_connections` slot until it closes. The absence of a warning on an idle
  close is not asserted by a test.
- The Ruby SDK suite was not run for this change. HTTP/2 flow control and the
  remaining body framing matrix are not part of this verification.
