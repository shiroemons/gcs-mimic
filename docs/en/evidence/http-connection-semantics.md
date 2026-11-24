# HTTP connection semantics verification

<!-- markdownlint-configure-file {"MD013": false} -->

Later change: `connection_timeout_ms` is now an alias, and `header_read_timeout_ms` (default 30000), `body_inactivity_timeout_ms` (default disabled) and `request_timeout_ms` (default 180000) replaced its single deadline. A full server now closes only the connection that turned idle first, once per waiting client, and only while a client waits (`full_server_closes_only_the_oldest_idle_connection_per_waiting_client`, `full_server_without_a_waiting_client_keeps_its_idle_connections`, `a_request_wakes_only_the_connection_that_turned_idle_first`). A request body on any route now waits at the client's pace under `body_inactivity_timeout_ms` instead of the request deadline, and the reply follows the limit that expired: a silent body answers JSON 408 `uploadBrokenConnection` or XML 400 `RequestTimeout`, so the XML 408 below now applies only to `object_request_timeout_ms` (`obj_http_stalled_resource_bodies_outlive_header_and_request_deadlines`, `obj_http_explicit_body_inactivity_answers_silent_resource_bodies`, `the_timeout_reply_names_the_limit_that_expired`). The record below keeps the values of its date.

Verified **2026-10-28** for AUDIT-010 at code commit `7564c5a` on `main`,
which gives keep-alive connections their own idle timeout on top of the
persistent connections from `a51400d`, the deadline responses and lingering
close from `89d9e9a` and the TLS trickle test fix from `c794ad8`.

| Command | Result | Evidence |
| --- | --- | --- |
| `env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/check.sh` | PASS, 45 test binaries and doctests, compatibility check, fmt and clippy including cross targets | Local macOS ARM64 run |
| `env -u GCS_MIMIC_GO_TEST_RUN env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-ruby` | PASS, pinned Go SDK suite in 3.9 s and the pinned Ruby 4.0.9 / google-cloud-storage 1.62.1 scenarios over HTTP | Local macOS ARM64 run |
| The same command with `GCS_MIMIC_SDK_SCHEME=https` | PASS, Go and Ruby over HTTPS with the exported local CA | Local macOS ARM64 run |
| GitHub Actions Foundation checks run 40646838968 | PASS, all eight jobs: pinned and stable Rust on Linux, native tests on macOS ARM64, Windows x86-64 and Linux musl, both container platforms and the Go SDK | <https://github.com/shiroemons/gcs-mimic/actions/runs/40646838968> |
| Server deadline and close changes reverted to their parent commit, updated tests run with `--no-fail-fast` | FAIL as intended for the six deadline and lingering tests listed below | Local negative check; source restored afterward |
| `.keep_alive(false)` restored temporarily, keep-alive test run | FAIL as intended: the second request on the connection reads EOF | Local negative check; source restored afterward |
| Server from `a51400d`, where idle shares the header deadline, with the idle test | FAIL as intended: the idle connection closed after 302 ms instead of the keep-alive timeout | Local negative check; source restored afterward |
| Slot reclamation disabled temporarily, full-server test run | FAIL as intended: the waiting client's read timed out | Local negative check; source restored afterward |

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
- After a response, a connection waits for `keep_alive_timeout_ms` (3 s in the
  test, longer than its 300 ms `connection_timeout_ms`) and closes without a
  warning; a next request head that starts and stalls closes after
  `connection_timeout_ms` with one `connection_timeout` warning and no
  `connection_failed` event
  (`keep_alive_idle_close_is_quiet_and_a_stalled_next_head_warns`). The ready
  event reports the setting, and CLI, environment and JSON precedence and
  bounds are tested with the other transfer timeouts
  (`transfer_timeout_precedence_and_bounds`).
- With `max_connections` of 1 and a 60 s keep-alive timeout, an idle
  connection gives its slot to a waiting client within two seconds and reads
  EOF (`full_server_reclaims_idle_keep_alive_slots_for_waiting_clients`).

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
- An idle connection closes as soon as every connection slot is taken,
  whether or not a client is waiting, so `max_connections` of 1 closes a
  connection after each response. A client that reuses a connection at the
  moment it closes still sees a closed connection.
- Bytes of a next request head that arrive together with the previous
  request's body count as idle time, so such a stalled head waits for
  `keep_alive_timeout_ms` instead of `connection_timeout_ms`.
- HTTP/2 flow control and the remaining body framing matrix are not part of
  this verification.

## Idle line order and request body silence, verified 2026-11-24

The scheduled stress run failed `full_server_closes_only_the_oldest_idle_connection_per_waiting_client` with "oldest stays": the waiting client closed a younger idle connection while the oldest kept its slot. A connection joined the idle line only when its watch task next saw the request finish, and on a busy runtime that task could run later for the connection whose response ended first. hyper drops a response body once its last bytes are buffered and before it flushes them, so since code commit `e04bddca` the body's drop takes the connection's place in line before its client can see the response end. The watch moves that place into the idle phase, and a request that starts before the watch runs gives it up.

Since `fefb9534` only a response that keeps its connection open takes a place: the request's end reads the response's `Connection` header as hyper does, so a timeout reply or an early rejection, which say `connection: close`, takes none, and neither does a request dropped before its response. Before that commit a timeout reply took a place, because the service marks it responded before hyper drops its body, and a waiting client's wake could land on the closing connection. Since `6073f7a2` the watch keeps such a connection out of the line too: the finished stage says whether the connection stays open, a closing connection waits for its reply in the closing reply phase that a timeout reply already used, bounded by the five-second close budget, and its end gives up a place an earlier request's end took. Before that commit the watch found no place and joined the line at the back, so a wake could still close the connection without the lingering close.

Validating those commits, the whole `it_admin` binary failed `direct_fixture_progress_renews_inactivity_without_disabling_the_deadline` in 1 of 12 runs with 408 `uploadBrokenConnection`, although its client sends a chunk every 30 ms under a 200 ms `body_inactivity_timeout_ms`. A request body stayed client-paced after each delivered frame, so the time the fixture waited to hand a chunk to its writer counted as the client's silence. Since `8e8abeb2` a delivered frame is the server's until it asks for the next one: that work counts against the request deadline, which every frame renews, and body inactivity starts only when the server waits on the client.

That change moves a held-up write from body inactivity to the request limit, so the reply to it becomes 503 `backendError` instead of 408. A one-time 500 ms pause before an upload hands a chunk to its writer, injected into every test of `it_protocol`, `it_json`, `it_xml_auth`, `it_admin` and `it_process`, failed only `obj_http_trickle_upload_deadline_releases_permits_without_publication` after the change and none before it: that test expects the 900 ms object deadline to answer 408 under a 300 ms request limit, and the same pause had answered 408 through body inactivity by accident. Since `67c43e13` it keeps the 300 ms header and body inactivity limits and runs with a 30000 ms request limit.

| Command | Result | Evidence |
| --- | --- | --- |
| `just check` on `67c43e13` | PASS in 441 s: fmt, workspace and cross-target Clippy, `cargo xtask lint`, 3935 tests with 0 failed and 1 ignored, and the documentation and script checks | Local macOS ARM64 run |
| `cargo test --locked --lib -- reclaim_tests server::ingress` on `67c43e13` | PASS, 23 tests: 18 reclaim tests, including `connections_stand_in_line_in_the_order_their_responses_ended`, `a_connection_whose_next_request_started_passes_its_wake_on` and `only_a_response_that_keeps_the_connection_open_takes_a_place_in_line`, 3 ingress tests, including `only_a_wait_on_the_client_counts_as_its_silence`, and 2 upload gzip decoder tests | Local macOS ARM64 run |
| `cargo test --locked --test it_process -- foundation` on `67c43e13` | PASS, 58 tests | Local macOS ARM64 run |
| `cargo test --locked --test it_admin -- direct_fixture_progress_renews_inactivity_without_disabling_the_deadline` on `67c43e13` | PASS | Local macOS ARM64 run |
| `cargo test --locked --test it_protocol -- obj_http_trickle_upload_deadline_releases_permits_without_publication` on `67c43e13` | PASS | Local macOS ARM64 run |
| The whole `it_admin` test binary run 12 times in a row, on `fefb9534`, `8e8abeb2` and `67c43e13` | 1 failure in 12 runs on `fefb9534`, in the direct fixture test above; 0 in 12 on each of the other two | Local macOS ARM64 run |
| GitHub Actions Timing stress run 45352682600 on `1852d7a7` | PASS on ubuntu-24.04 and macos-15, two rounds each of 15 test binaries and doctests run 3 and 4 at a time beside as many busy loops | <https://github.com/shiroemons/gcs-mimic/actions/runs/45352682600> |
| A 300 ms sleep injected once into the watch's handling of a finished request, with the fix, full-server test run three times | PASS 3 of 3 | Local negative check; source restored afterward |
| The same sleep with the watch joining the line itself, as before the fix | FAIL as intended 3 of 3, with "oldest stays" | Local negative check; source restored afterward |
| `only_a_response_that_keeps_the_connection_open_takes_a_place_in_line` with the earlier `Expired` stage check restored | FAIL as intended at the timeout reply: the idle connection queued after it did not receive the waiting client's wake | Local negative check; source restored afterward |
| A 300 ms sleep injected once before the direct fixture hands a chunk to its writer, direct fixture test run three times | FAIL 3 of 3 with 408 `uploadBrokenConnection` before `8e8abeb2`; PASS 3 of 3 with it | Local negative check; source restored afterward |
| `only_a_wait_on_the_client_counts_as_its_silence` with a delivered frame leaving the body pending again | FAIL as intended at the first frame | Local negative check; source restored afterward |
| `only_a_response_that_keeps_the_connection_open_takes_a_place_in_line` with a closing end turning idle in the watch, and again with a closing end keeping an earlier place | FAIL as intended at the timeout reply both times | Local negative checks; source restored afterward |
| The 500 ms upload pause above, over the five test binaries | 0 failures before `8e8abeb2`; 1 after it, the trickle test, which passes 3 of 3 with the pause since `67c43e13` | Local negative check; source restored afterward |
