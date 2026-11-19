# Shared modules outside the JSON adapter

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the shared request error, bucket creation, upload and media headers, and the streaming multipart parser moved out of the JSON adapter into neutral modules, in commits `3193802b`, `91f499e6`, `e474a8f7` and `84aeff2c`. Verified on 2026-11-19 at the final tree of `84aeff2c`; `src/` and `tests/` have changed since only in lint attributes. This is a refactor: no feature changes status or fidelity.

## What moved

| Old test ID | Where its assertions live now |
| --- | --- |
| `json::multipart::tests::multipart_parser_bounds_preamble_epilogue_headers_and_metadata` | Split. The framing caps (preamble, epilogue, headers) are in `multipart::tests::multipart_parser_bounds_preamble_epilogue_and_headers`; the JSON metadata cap is in `json::multipart::tests::related_uploads_bound_the_metadata_part` |
| `json::multipart::tests::related_uploads_accept_the_gcloud_lf_framing_and_quoted_boundary` | Split. The framing and quoted-boundary assertions are in `multipart::tests::related_uploads_accept_the_gcloud_lf_framing_and_quoted_boundary`; the gcloud metadata assertions are in `json::multipart::tests::related_uploads_accept_the_gcloud_lf_framing` |
| `json::object_tests::obj_render_002_media_headers_match_selected_generation_and_hashes` | `object::media_headers::tests::obj_render_002_media_headers_match_selected_generation_and_hashes` |
| `json::error::tests::backend_causes_are_actionable_and_private_details_stay_private` | `api_error::tests::backend_causes_are_actionable_and_private_details_stay_private` |

The two new `json::multipart::tests::` IDs did not exist before `e474a8f7`. Verification records dated earlier than 2026-11-19 do not name them; the ledger names them only in acceptance cases and row-level test lists, and the run below is their first verification record.

## Commands and results

| Command | Result |
| --- | --- |
| `cargo test --lib json::multipart::tests::related_uploads` | PASS, 4 tests, including `related_uploads_bound_the_metadata_part` and `related_uploads_accept_the_gcloud_lf_framing` |
| `cargo test --all-targets --no-fail-fast` | PASS, 0 failures in every target: library unit tests 2581; binary unit tests 71; `it_admin` 91 (1 ignored); `it_import_seed` 96; `it_json` 142; `it_objects` 180; `it_process` 131; `it_protocol` 161; `it_sync` 33; `it_xml_auth` 327 |
| `cargo test -p xtask` | PASS, 294 tests (277 + 13 + 3 + 1) |
