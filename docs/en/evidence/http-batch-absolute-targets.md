# JSON batch parts with absolute request lines: evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [slice](../slices/http-batch.md), feature BEHAVIOR-COMMON-008,
acceptance case `batch-absolute-origin`. Verified 2026-10-31.

## Observed contract

- The Java client (`google-cloud-storage` 2.76.0 with `google-api-client`
  2.9.2) writes each `storage.batch()` part as an absolute request line, for
  example `GET https://<endpoint>/storage/v1/b/<bucket>/o/<object> HTTP/1.1`.
  gcs-mimic previously rejected the whole envelope with 400.
- Cloud Storage, observed 2026-10-31: `POST /batch/storage/v1` mixing a part
  with an absolute `https://storage.googleapis.com/storage/v1/...` request line
  and a relative part returned an outer 200; the parts answered 200 and 404,
  the same as equivalent relative parts.
- The [batch guide](https://docs.cloud.google.com/storage/docs/batch) says
  request lines "must only contain the path portion of the URL; full URLs can
  have undefined behavior", and that a request that cannot be parsed into
  sub-requests returns 400. Absolute URLs naming another origin were not
  observed.

## Local decision

An absolute part is accepted only when its scheme and authority equal the
advertised API endpoint (`api_base_url`) after the public-origin normalization,
and its path continues below the advertised path prefix with `/storage/v1/`.
Any other origin is an envelope parse failure: 400 for the whole batch before
any part runs. A per-part error would still execute the other parts of an
envelope addressed partly elsewhere, and the local dispatcher must never treat
an inner URL as a route to another server. The listener address and request
`Host` are not consulted.

## Acceptance cases

`tests/http_batch.rs`:

- `batch_accepts_absolute_targets_only_on_the_advertised_api_origin`: with
  `--api-base-url http://gcs.invalid:4443`, absolute parts in three case
  spellings answer 200 beside a relative 404 part. Another host, `https`
  scheme, another port, an omitted port, an IP literal, userinfo (with and
  without a password, and `origin@other`), `ftp`, an empty path, internal,
  nested batch, upload, `alt=media`, `//storage/v1` and a fragment each return
  400, and an earlier DELETE part in the same envelope does not run.
- `batch_absolute_targets_include_the_advertised_path_prefix`: with
  `--api-base-url https://proxy.invalid/gcs/`, `https://proxy.invalid:443/gcs/storage/v1/...`
  answers 200; a missing, extended (`/gcsx`) or nested prefix returns 400.

Before the fix both tests failed with the outer 400 "Batch parts require
relative Google JSON API paths".

## Commands

| Command | Result |
| --- | --- |
| `cargo check` | PASS |
| `cargo test --test http_batch` | PASS: 6 passed, 0 failed |
| `cargo clippy --all-targets --locked -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: 0 production violations |
| `cargo xtask compatibility check` | PASS |
| SDK scorecard Java probe `sdk.storage_batch` (release build behind an HTTPS front, `api_base_url` set to that front) | PASS |

## Limits

- Foreign-origin behavior of Cloud Storage is unobserved; the local 400 is a
  safety choice, not a parity claim.
- An advertised host that the public-origin rules reject (for example a name
  containing an underscore) matches no absolute part; relative parts still work.
