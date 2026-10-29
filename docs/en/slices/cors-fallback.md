# CORS fallback for buckets without rules

<!-- markdownlint-configure-file {"MD013": false} -->

DESIGN-100 adds one emulator setting for callers migrating from
fake-gcs-server. It is a local migration aid, not a Google Cloud Storage
behavior: Cloud Storage has no server-wide CORS policy.

## Why

fake-gcs-server v1.56.1 (commit `896ece51e49eed0946089f3089f6343855c0d106`)
wraps every route in one `gorilla/handlers` CORS middleware:

- allowed origins `*`, answered as `Access-Control-Allow-Origin: *`;
- methods HEAD, GET, POST, PUT, PATCH and DELETE;
- request headers Accept, Accept-Language, Content-Language and Origin, plus
  Content-Type, Content-Encoding, Range and Content-Range, plus the
  comma-separated `-cors-headers` / `FAKE_GCS_CORS_HEADERS` list;
- a preflight requesting any other header receives 403;
- exposed `Location`, `Access-Control-Allow-Credentials: true` and no max age;
- bucket `cors` is accepted by PATCH but not stored or applied.

Sources: [server middleware](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go),
[flag parsing](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go)
and [gorilla/handlers CORS](https://github.com/gorilla/handlers/blob/main/cors.go).

gcs-mimic follows the [Cloud Storage CORS contract](https://docs.cloud.google.com/storage/docs/cross-origin):
JSON endpoints already allow any origin and every valid requested header, while
XML/public object routes use stored bucket rules. A page that fetched
path-style public URLs from fake-gcs-server without configuration therefore
fails CORS against a gcs-mimic bucket without rules. Browsers report only a
generic CORS failure, so the server must name the fix.

## Setting

| CLI | Environment | JSON config | Values |
| --- | --- | --- | --- |
| `--cors-fallback MODE` | `GCS_MIMIC_CORS_FALLBACK=MODE` | `"cors_fallback": "MODE"` | `none` (default) or `allow-all` |

The setting chooses what XML/public CORS does for a bucket without stored
rules. It works in both `native` and `legacy-emulator` profiles, and the ready
event reports `cors_fallback`. Like every native option it takes a value; a
bare `--cors-fallback` fails with `--cors-fallback allow-all` as its example.

Server-wide CORS rules are deliberately not configurable. Production buckets
need their own `cors`, so rules belong on buckets, through the JSON API or the
[buckets file](bucket-initialization.md), where local tests exercise the same
placement. A header-list setting is also absent: `allow-all` already allows
every syntactically valid requested header, a superset of the source list.

`FAKE_GCS_CORS_HEADERS` and `-cors-headers` stay rejected at startup. The
advice starts with removal, because JSON-only callers need nothing else and
`allow-all` departs from Cloud Storage; only callers whose browsers read
XML/public URLs of buckets without stored rules are told to also set
`GCS_MIMIC_CORS_FALLBACK=allow-all`. The supplied header names are not echoed.

## Allow-all behavior

`allow-all` applies to path-style and virtual-host XML/public object routes,
including signed URLs, when the bucket has no stored CORS rules or does not
exist. A bucket with stored rules keeps its exact rule behavior, including
denials. JSON and gRPC behavior is unchanged.

| Case | Response |
| --- | --- |
| Preflight with a valid Origin and request method DELETE, GET, HEAD, POST or PUT | Allow-Origin `*`, Allow-Methods `HEAD, GET, POST, PUT, DELETE`, requested headers echoed when every name is valid, preflight Vary |
| Actual request with a valid Origin | Allow-Origin `*`, Expose-Headers `Location`, Vary `Origin` |
| Missing bucket | Same headers on the XML 404, so the page can read the error |
| No Origin, malformed or repeated Origin, PATCH or another method, missing request method, malformed requested headers | No CORS headers, as for bucket rules |

Differences from the source middleware are deliberate. No credential header is
sent, because browsers reject credentials with a wildcard origin. No route
returns 403 for a header, because every valid header is allowed. Health,
readiness and other emulator routes receive no CORS headers. PATCH is not
offered because no XML route accepts it. CORS never grants access: strict
authentication, IAM, ACLs and public access prevention still decide the actual
response.

## Unconfigured warning

With `none`, an XML/public request that carries a valid Origin and reaches an
existing bucket without rules logs one `cors_unconfigured` warning for that
bucket at `warn` level:

```json
{"event":"cors_unconfigured","level":"warn","bucket":"assets","message":"XML/public browser request reached a bucket without CORS rules; set GCS_MIMIC_CORS_FALLBACK=allow-all or configure the bucket cors field"}
```

The process remembers at most 64 warned bucket names. Requests without Origin,
missing buckets and buckets with rules do not warn. The warning does not change
the response.

## Acceptance

| Case | Tests |
| --- | --- |
| Configuration values, precedence, both profiles and bare-flag example | `config::tests::cors_fallback_defaults_to_none_with_precedence_in_both_profiles` |
| Legacy header-list advice starting with removal, without echoing values | `config_input::tests::legacy_advice_uses_values_precedence_defaults_and_redacts_unsafe_urls` |
| Allow-all headers, stored-rule precedence and denials | `xml::cors::tests::allow_all_fallback_allows_any_valid_origin_and_header_only_without_bucket_rules` |
| Bounded once-per-bucket warning memory | `xml::cors::tests::unconfigured_warning_is_once_per_bucket_and_bounded` |
| Server preflight, GET/HEAD, missing bucket, stored rules, unchanged JSON and ready field | `allow_all_fallback_covers_unconfigured_xml_buckets_without_overriding_rules_or_json` |
| Server warning emitted once per bucket and not for Origin-less requests | `unconfigured_xml_cors_warns_once_per_bucket_without_the_fallback` |
| Ready event field set and default | `process_shutdown_reports_stopped_and_releases_catalog_for_restart`, `api_origin_ready_and_resumable_locations_use_bound_addresses` |

Verification: [CORS fallback evidence](../evidence/cors-fallback.md).
