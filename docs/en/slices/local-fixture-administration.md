# Direct local fixture administration

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the direct fixture subset of DESIGN-062 and DESIGN-068. This extension
supports Go embedding adapters that previously supplied fake-gcs-server
`InitialObjects` or `CreateObject`. It is not a Google API operation, a reset,
directory-wide atomic reseed, or a RAM backend.

## Contract and sources

Checked on 2026-10-30:

- [Google Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects) supplies ordinary field names, string-encoded generations and checksums. Generation and creation timestamps supplied here are an emulator-only fixture extension; ordinary Google uploads do not accept these identity overrides.
- [Reference ObjectAttrs](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go) at fake-gcs-server v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`, permits manually injected generation and timestamps. Its permissive replacement behavior does not override this implementation's immutable generation identity or integrity checks.
- The checked-in [source manifest](../../../compatibility/upstream/source-storage-contract.json) retains the pinned comparison source and format digests. The existing [import identity contract](shared-imported-identity.md) owns durable source-generation publication.

No dependency is added. The integration owner's slice-start freshness check
verified Rust 1.99.1 and all direct Rust dependencies CURRENT. Its all-scope
check exited 1 for the already recorded Composer latest-index publication
exception, with review due 2026-11-06; the release-scope check passed.

Fixture wire acceptance exposed an existing representation gap: the catalog
already preserved source microseconds, but the shared JSON timestamp renderer
discarded them on POST and GET responses. This slice extends that renderer to
at most six fractional digits, retaining the previous three digits for exact
millisecond values. Values floor toward negative infinity at microsecond
precision; timezone, valid year range and millisecond-derived bucket ETags
remain unchanged. XML HTTP dates and signing basic timestamps still select
whole seconds. This is a source-preservation extension of the historical
[catalog timestamp representation](catalog-timestamp-precision.md).

The timestamp parser, identity validator and renderer now share the same UTC
range, including timezone normalization. Publication also validates the complete
record and computed bucket-retention expiration before retiring the preceding
generation or advancing the allocator. Setting bucket retention checks existing
creation times in that same transaction. These checks reject an offset beyond
year 9999 or a retention expiry in year 10000 with 400 before any mutation,
instead of committing an object that subsequently fails metadata rendering.
The shared parser also protects custom-time, object-retention and source-import
inputs. The native historical year-0000 lower boundary remains unchanged.

## Startup authority and wire format

Administration is disabled by default. `--admin-enabled true` causes catalog
open to create a private local administrative grant. A request can only clone
that stored capability; changing runtime endpoint configuration cannot mint one.
The normal `Catalog::open` and `open_with_clock` entry points issue no grant.

In strict mode, configure `--admin-principal ID` and authenticate with a bearer
token for that registered principal. This exact principal selection is separate
from Google IAM: `roles/storage.admin` alone grants no fixture administration,
and the selected manager gains no Google API permissions. Permissive mode allows
an enabled management request without credentials. Both modes require listener
isolation as with the existing local configuration endpoint.

Send `POST /_internal/objects` with `Content-Type: multipart/related; boundary=B`.
The first part is an `application/json` object of at most 64 KiB; the second is
the streamed media body. Existing multipart limits, optional trailing checksum
parts, strict framing and complete transport EOF apply. Query parameters are
rejected. The target bucket must already exist; missing buckets return 404.

Required JSON fields are `bucket` and `name`. Supported optional fields:

- `generation`: a decimal JSON string from 1 through `i64::MAX - 1`. Omission uses the ordinary persistent allocator. Explicit identities advance the high-water mark, and an existing same-name, same-generation resource returns 409 without replacing bytes. Other live replacements obey ordinary bucket versioning, holds and retention.
- `created` or `timeCreated`, plus `updated`: complete RFC3339 timestamps with exact microsecond precision. The creation aliases are mutually exclusive. Omitted times use the injected clock, truncated to the catalog's microsecond precision; nonzero sub-microsecond digits are rejected instead of discarded.
- `contentType`, `contentEncoding`, `contentDisposition`, `contentLanguage`, `cacheControl`, `metadata`, `customTime` and `storageClass`: the existing shared upload validators and bucket/Autoclass constraints apply. The media part's content type is the fallback when metadata omits it.
- `md5Hash` and `crc32c`: supplied checksums must match the independently computed streamed body. Omission never disables hashing.
- `notificationMode`: `emit` by default, or `suppress` for trusted initialization. This local administrative field is unavailable on Google API routes. Suppression covers both retirement and finalize selection in the publication transaction; it changes no authorization, conditions, hashes, generation allocation or synchronization journal. Go constructor InitialObjects uses suppression; later direct helper calls retain emission.

ACLs, object retention, temporary holds and event-based holds now use shared
validation and atomic publication; see [atomic fixture replay](atomic-seed-reseed.md).
UBLA/PAP and enabled-bucket retention restrictions apply. Deleted/archive
identities, caller-supplied ETags, sizes, unknown fields and other unmodeled attributes are rejected with 400.
They must not be silently dropped by an embedding adapter. Bucket defaults still
apply through shared publication, and existing protection is never bypassed.

Success returns HTTP 200 and the ordinary JSON Object resource. The route
keeps the existing two-slot body channel and 64 KiB chunks; neither the whole
body nor the object catalog is collected. All metadata and identity checks commit
with publication in one short transaction after full framing and checksums.
Failed input or storage operations retain the preceding object. Blob durability,
cleanup and restart use the existing publication/recovery protocol.

## Acceptance and verification

The acceptance tests are in `tests/admin_http.rs`:

| Test | Observable contract |
| --- | --- |
| `direct_fixtures_preserve_metadata_generation_and_times_across_restart` | Explicit identity, content fields/class, exact microsecond timestamps, checksums, conflict preservation, subsequent automatic high-water allocation and restart equality |
| `direct_fixture_rejections_preserve_existing_bytes` | Invalid generations/times/class, unsupported metadata, bad checksums, excessive body and incomplete framing preserve the original metadata and bytes |
| `initial_fixture_notification_mode_is_request_local_and_preserves_runtime_events` | Initial overwrite emits no JSON-configured notifications, ordinary helper/API writes retain old-generation and finalize events, and invalid modes leave bytes and queued messages unchanged |
| `direct_fixture_management_is_opt_in_and_independent_of_google_iam` | Default disabled, strict anonymous/unknown/IAM-only caller denial, exact configured principal acceptance and no resulting API grant |
| `direct_fixture_progress_renews_inactivity_without_disabling_the_deadline` | A 600 ms progressing upload with 30 ms chunks succeeds under a 200 ms inactivity timeout and a separate 5 s absolute deadline |
| `direct_fixture_retention_expiry_range_is_validated_in_the_publication_transaction` | UTC-normalized range and computed retention-expiry rejection preserve old bytes, metadata, listing and allocator; adding retention cannot make an existing object unreadable |

Native verification on 2026-10-30: `cargo check --locked --offline --tests`
passed, `cargo test --locked --offline --test admin_http` passed all 17 cases
including the fixture and initial reset cases, and
`cargo test --locked --offline --lib -- imported_identity timestamp import::codec`
passed 18 cases. `cargo xtask lint` reported zero production violations.
Both local-administration slice documents passed Markdown lint. Final SDK and
container runs belong to the integrated source and are not duplicated here.

The notification-mode follow-up passed `cargo check --locked --offline --tests`
and `cargo test --locked --offline --test admin_http fixture` with all six
selected cases, including strict rejection for both emission modes. The outbox
unit contract separately covers disk/memory, bucket versioning, global and JSON
destinations, startup seed, old-generation retirement and failed checksum or
condition preservation.

## Remaining scope

The [atomic replay slice](atomic-seed-reseed.md) adds reseed and fixture ACL/retention/holds,
while the [memory backend](memory-backend.md) owns process-local storage.
Seed metadata sidecars, direct fixture deleted versions and wider source history remain separate work. The broad ledger
rows retain their incomplete acceptance cases. No performance result, memory
backend equivalence or provider-exact administrative errors are claimed.
