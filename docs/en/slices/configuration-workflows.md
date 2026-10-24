# Configuration workflows contract

<!-- markdownlint-configure-file {"MD013": false} -->

Contract established on **2026-10-24** for `configuration-workflows`; independent
review results remain in the ledger. The legacy-input diagnostics and public-origin slices now
implement the input/preflight, shared diagnostic and public Host/helper contracts below; reserved
settings remain unavailable until their named slices deliver effective behavior.
DESIGN-055 and UX-003 remain PARTIAL; DESIGN-064 and DESIGN-065 retain their
implemented precedence and single-listener TLS scopes. The
[feature ledger](../../../compatibility/features.json) owns status and evidence.
The [current configuration reference](../fake-gcs-server-configuration.md)
continues to describe the shipping binary.

## Baseline and boundaries

The configuration/startup findings in
[`planning/onboarding-tracks-findings.ja.md`](../../../planning/onboarding-tracks-findings.ja.md#2-設定と起動)
were checked against `src/config.rs`, `src/main.rs`, `src/url.rs`,
`src/xml/mod.rs`, `src/server.rs` and the Compose files. They still describe the
baseline implementation: first-error validation, ignored `FAKE_GCS_*`, one
listener, a fixed default advertised port, no stored public origin and no log
level. The diagnostics implementation replaces the first two limitations and
public-origin configuration adds the separately stored public origin, while
preserving the catalog and config → material → catalog → bind startup order.

This contract follows the decisions in
[`planning/onboarding-tracks-plan.ja.md`](../../../planning/onboarding-tracks-plan.ja.md#4-configuration).
Refinements to its design requirements are explicit below: settings described
there as having no native equivalent remain unsupported until their named
slice implements an effective replacement. Reserving a name below does not make
it accepted. JSON CORS header additions are unnecessary under the existing
allow-all-valid-request-header contract. The global legacy CORS setting remains
unsupported: JSON callers can remove it, while XML/public-URL callers must set
bucket CORS `responseHeader` entries. This protocol-specific advice avoids an
ineffective global replacement and preserves bucket-specific XML behavior.
Memory and notification delivery remain separate plans.

The pinned comparison is fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`; its source archive SHA-256 is
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.
The existing [source configuration evidence](../evidence/fake-gcs-server-configuration.md)
records executed parser probes for all 19 variables. The
[Host evidence](../evidence/configuration-host-matching.md) records native-binary
wire probes for authority, DNS, routing precedence and request forms.

Google [request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints)
and [canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests)
remain the API/signing contract. Legacy routing is an explicit comparison
profile, not Google fidelity. Existing SPEC-XML-VIRTUAL-HOST and
SPEC-SIGNING-EDGES in `compatibility/gaps.json` stay open; no provider question is
resolved by this local configuration contract. No Discovery/protobuf field,
permission or storage transition is added.

## Inputs, precedence and validation

Server settings keep **CLI > `GCS_MIMIC_*` environment > JSON file > defaults**.
CLI options are `--kebab-case VALUE` pairs, file keys are `snake_case` and
environment names are `GCS_MIMIC_UPPER_SNAKE_CASE`. `--config` overrides
`GCS_MIMIC_CONFIG`. Files remain flat JSON objects capped at 64 KiB, with string
or unsigned-integer values. Unknown native keys, repeated CLI options, malformed
values and out-of-range values fail; empty strings do not mean unset except for
the existing empty virtual-domain list. Validate effective values after merging;
unknown keys and legacy-input diagnostics cannot be hidden by a native override.
Malformed/unreadable files are reported even when a CLI override exists.

The following existing defaults and validation remain unchanged:

| Setting | Default | Validation/role |
| --- | --- | --- |
| `listen` | `127.0.0.1:4443` | IP socket address; port 0 allowed; binds only |
| `scheme` | `https` | Currently `http` or `https`; future `both` below |
| `tls_cert`, `tls_key` | Bundled public development pair | Both custom nonempty paths or neither; PEM size/trust rules in [TLS contract](local-tls.md) |
| `tls_handshake_timeout_ms` | `5000` | `1..60000`; HTTP rejects TLS settings |
| `data_dir` | `data` | Nonempty; native catalog/blob destination, not a legacy root |
| `seed_dir` | Unset | Nonempty when supplied; one-time seed input |
| `seed_project` | `local-project` | Existing project validator |
| `compat_profile` | `native` | `native` or `legacy-emulator`; independent of authentication |
| `auth_mode`, `identity_file` | `permissive`, unset | `strict` requires a nonempty registry path; permissive rejects a supplied registry |
| `xml_virtual_host_domains` | Empty/disabled | At most 32 distinct validated DNS names; existing [virtual-host rules](xml-virtual-host.md) |
| `max_connections` | `32` | `1..1024`; future dual listeners share this total |
| `max_object_bytes` | `1073741824` | `1..5497558138880` bytes |
| `worker_threads`, `max_blocking_threads` | `2`, `2` | Each `1..32` |
| `connection_timeout_ms`, `shutdown_timeout_ms` | `5000`, `5000` | Each `1..60000` |
| `object_request_timeout_ms` | `900000` | `1..86400000` |
| `catalog_wait_timeout_ms` | `30000` | `1..60000` |

### Endpoint roles and reserved settings

| Setting | Contract/default after its implementing slice | Validation and observable effect |
| --- | --- | --- |
| `api_base_url` | Explicit fixed base, otherwise derive once after bind (`config-api-origin-default`) | Absolute HTTP(S) URL without userinfo, query or fragment; trim trailing slashes; preserve a proxy path prefix. Explicit value wins even if its scheme differs from the listener. All selfLink/mediaLink/resumable Location values use this one base; never request Host/Forwarded. |
| `public_base_url` | Unset (`config-public-origin`) | `--public-base-url` / `GCS_MIMIC_PUBLIC_BASE_URL`; absolute HTTP(S) origin with optional port and root path only; no userinfo/query/fragment. Remove root/trailing slashes, lowercase DNS, omit scheme-default ports (HTTP 80/HTTPS 443), preserve other ports and bracket IPv6. Normalization is idempotent. Supplies the three effects below. |
| `listen_http` | Only for `scheme=both`: primary bind IP with port `8000` (`config-dual-listener`) | `--listen-http` / `GCS_MIMIC_LISTEN_HTTP`; IP socket address, port 0 allowed; reject when scheme is not `both`. Primary `listen` serves HTTPS, second listener HTTP. Bind failure on either prevents ready and releases both listeners. |
| `log_level` | `info` (`config-log-level`) | `--log-level` / `GCS_MIMIC_LOG_LEVEL`; `debug`, `info`, `warn`, `error`, accepting `warning` as `warn`. Reject other levels. Filtering must affect emitted events; fatal validation diagnostics and stdout lifecycle events remain visible. |
| `default_location` | Native `US` (`config-remaining-settings`) | `--default-location` / `GCS_MIMIC_DEFAULT_LOCATION`; only existing supported `US`, `EU`, `ASIA`. Applies when creating a native bucket without explicit location; explicit API/initializer location wins. Never rewrites existing buckets or claims regional placement. Explicit use in `legacy-emulator` is rejected until effective legacy location support exists. |

The location subset is deliberately native-only: the current legacy profile
rejects create-time locations other than case-insensitive `US-CENTRAL1`
(including `us-central1`); it accepts that value
without preserving it, stores `US` and renders fixed `US-CENTRAL1`.
Accepting a new global default in that profile would therefore have no effect.
Without the new setting its existing behavior stays unchanged; with it, report
unsupported and recommend `compat_profile=native` with, for example,
`default_location=EU`. Broader legacy regional configuration remains a separate
BEHAVIOR-BUCKET-003 acceptance scope, not a silent mapping of `US-CENTRAL1` to US.

For an implicit API base, use the primary listener's actual bound port, including
port 0; replace `0.0.0.0` with `127.0.0.1` and `::` with `[::1]`. Preserve
other bound IPs, bracket IPv6 and use HTTPS for `both`. Resolve the base before
constructing `State` and ready. `config-api-origin-default` owns changing
`Config.api_base_url` to distinguish explicit/unset. During preflight, check
API-host collisions using the explicit base's host or the listen IP after the
wildcard-to-loopback substitution; this host is known before bind and collision
matching ignores ports. A collision must prevent catalog access, bind and ready,
including numeric DNS suffixes such as `0.1` matching `127.0.0.1`. Only fill in
the actual bound port immediately after bind in `main.rs`, before constructing
`State` or ready. This is value
resolution within the existing bind-to-State boundary, not a startup-phase
reordering; it does not depend on `shared-startup-phases`. Shared state owns
any later phase restructuring and preserves this boundary. Compose overrides for HTTP and
HTTPS must use their actual host-published port, for example
`https://localhost:${GCS_MIMIC_PORT:-4443}`. One fixed origin cannot serve unrelated
host/peer/browser address spaces; select one mutually reachable name or separate
instances. Proxy-derived origins/allowed-host policy remain DESIGN-056 work.

`public_base_url` has exactly these effects:

1. Its DNS hostname joins the XML virtual-domain list. The union is capped at 32;
   one occurrence already in the explicit list is merged, while duplicates
   within that list remain errors. Existing longest-suffix and API-host conflict
   checks apply to native routing; legacy opt-in routing follows the measured
   rules below. IP literals skip only this effect, with a warning
   diagnostic and ready field `public_virtual_host_enabled:false`.
2. Its authority supplies opt-in legacy Host matching as specified below. IPs
   remain usable here. Native path-style acceptance is unchanged.
3. Offline `url public` / `url sign` may read the JSON file's `public_base_url`
   only when **`--config PATH` is explicit**. `--base-url` wins over that value;
   if neither supplies a base, fail with an actionable diagnostic. Do not read
   `GCS_MIMIC_CONFIG`, server environment or default config files in these
   helpers, open the catalog, load server TLS/registry files or contact a server.
   Check readability, the 64 KiB cap, JSON object syntax, flat string/unsigned
   integer value types and the file's `public_base_url` syntax when present,
   including when overridden. Ignore other keys (including unknown keys) and
   server-only value/cross-field rules such as strict/identity or TLS pairs.
   Validate `--base-url` with the helper's existing path/virtual-style rules.
   Update helper help text to state this explicit-file exception.

Public origins are retained as a validated `PublicOrigin` with private fields;
requests reuse its parsed authority and host kind rather than parsing a string
and silently disabling routing on failure. One selected bucket/raw object path
is tied to its URI and reused for CORS and XML dispatch. Public-origin port zero
is rejected with a reachable-port example such as `:4443`; listener port zero
and API-base port policy belong to their separate contracts.
Public-origin DNS validation rejects
noncanonical numeric host spellings such as `127.1`, `0x7f.0.0.1` and
`2130706433`: use canonical `127.0.0.1` instead. More generally, a final DNS label
containing only digits or `0x`/`0X` followed by hex digits is rejected unless the
whole host is already canonical IPv4. This follows the
[WHATWG numeric-host boundary](https://url.spec.whatwg.org/commit-snapshots/845bafca22fb0c5a42c4bf384a9bdea525a2f10d/#concept-ends-in-a-number).
It prevents browsers from interpreting a configured DNS suffix as IPv4 and
changing the final authority after URL construction or signing. This is an
explicit validation refinement, not a change to native arbitrary-Host routing
or existing explicit XML-domain validation.

Ready currently always includes `public_base_url` as a normalized string or
JSON null, and `public_virtual_host_enabled` as a boolean (false when absent or
IP). `config-public-origin` owns these two keys. `api_base_url` is not yet a
ready key: `config-api-origin-default` will add its effective string after
implementing bound-address resolution. Shared state's `ReadyEvent` retains the
implemented keys. No setting creates DNS/certificates, grants IAM/ACL access or changes
PAP. Helpers encode/sign the final host, port, path, query and required headers;
there is no post-signing Host replacement. An IP public base supports path-style
helpers only; virtual style still requires DNS. Supplied HTTPS certificates must
cover the final hostname. Dual-listener ready retains the existing primary
`listen` key and adds `listeners:[{"listen":"...","scheme":"https"},{"listen":"...","scheme":"http"}]`.

Ready reports URL roles, not a guarantee of SDK XML-read reachability. With
legacy matching enabled, XML path-style requests to the API authority also
fall back to a hostname bucket when that authority does not match the public
authority. For SDKs that read XML (including the Go SDK's default), configure
the public base with the same authority as the SDK's API endpoint. Compare the
configured API endpoint (or an object's JSON `selfLink`/`mediaLink` authority)
with `public_base_url` in ready; do not assume the API authority bypasses legacy
routing. Comparing two ready URL fields becomes possible only after
`config-api-origin-default` adds the API key.

## Disposition of all 19 source environment variables

All rows are explicit migration instructions, **not aliases**. In
`config-legacy-input-diagnostics`, the presence of any known variable, including
an empty value, fails startup even if native equivalents are also supplied.
Future native capability delivery does not start accepting the old name.
Detect legacy option-name tokens across argv before subcommand dispatch,
including an argv beginning with flags after a Compose `command` override.
Use the native pair grammar to identify name positions and exclude native
option values: `url public --object -data` and `url sign --object -port`
contain valid object names, not legacy flags. Equals forms consume only their
own token and must not shift subsequent name/value positions.
Recognize `-name VALUE`, `--name VALUE`, `-name=VALUE`, and `--name=VALUE`
without pairing unrelated tokens or shifting after an equals form. The 19 names
are `port`, `port-http`, `backend`, `filesystem-root`, `public-host`,
`external-url`, `scheme`, `host`, `data`, `cors-headers`,
`event.pubsub-project-id`, `event.pubsub-topic`, `event.bucket`,
`event.object-prefix`, `event.list`, `location`, `cert-location`,
`private-key-location`, and `log-level`. Unknown single-hyphen options receive
native syntax guidance. A native `--name VALUE` spelling takes precedence when
identical (`serve --scheme VALUE`, and `serve --log-level VALUE` once delivered);
the indistinguishable valid native form must not be rejected as legacy. An
equals form still receives native pair-syntax advice. Without `serve`, suggest
the complete native command as well as reporting all detectable legacy inputs.

Compose `command` replaces the image CMD. Its migration advice must therefore
include `serve --listen 0.0.0.0:4443 --data-dir /var/lib/gcs-mimic` (or the
operator's effective bind/volume choices); otherwise the native loopback default
and relative data directory replace the image's usable listener/persistence.

| Source variable | Final native disposition and example fix | Owner |
| --- | --- | --- |
| `FAKE_GCS_PORT` | Combine effective legacy HOST/PORT, e.g. port `8080` alone yields `GCS_MIMIC_LISTEN=0.0.0.0:8080`; no `GCS_MIMIC_PORT` server alias | Diagnostics |
| `FAKE_GCS_HOST` | Combine effective legacy HOST/PORT, e.g. host `127.0.0.2` alone yields `GCS_MIMIC_LISTEN=127.0.0.2:4443` | Diagnostics |
| `FAKE_GCS_SCHEME` | `GCS_MIMIC_SCHEME=https` or `http`; `both` only after dual listeners | `config-dual-listener` |
| `FAKE_GCS_PORT_HTTP` | Unsupported today; future `GCS_MIMIC_SCHEME=both` with `GCS_MIMIC_LISTEN_HTTP=0.0.0.0:8000` | `config-dual-listener` |
| `FAKE_GCS_EXTERNAL_URL` | Use its supplied safe URL as `GCS_MIMIC_API_BASE_URL`, preserving the client-reachable prefix; redact credential-bearing URLs | Diagnostics |
| `FAKE_GCS_PUBLIC_HOST` | Use `GCS_MIMIC_PUBLIC_BASE_URL=https://storage.example.invalid:4443` including the scheme; legacy Host matching also needs `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`. Native path-style accepts any Host. For legacy SDK XML reads, use the API endpoint's authority too: a different public authority makes API-host XML reads fall back to a hostname bucket | `config-public-origin` |
| `FAKE_GCS_DATA` | `GCS_MIMIC_SEED_DIR=fixtures`; native seed is one-time and does not overwrite on restart | Diagnostics; Import owns richer seed fidelity |
| `FAKE_GCS_FILESYSTEM_ROOT` | Native destination `GCS_MIMIC_DATA_DIR=data` has a different format; retain the source and use the separately delivered Import conversion, never mount the old root as native data | Diagnostics; Import |
| `FAKE_GCS_BACKEND` | Unsupported selector; disk-backed native catalog remains default. Bounded memory mode needs a separate DESIGN-077 slice; no claim that `memory` works | Separate plan |
| `FAKE_GCS_CERT_LOCATION` | `GCS_MIMIC_TLS_CERT=certs/server.pem` together with `GCS_MIMIC_TLS_KEY=certs/server-key.pem` | Diagnostics |
| `FAKE_GCS_PRIVATE_KEY_LOCATION` | Matching `GCS_MIMIC_TLS_KEY=certs/server-key.pem` and TLS_CERT; one-sided overrides fail | Diagnostics |
| `FAKE_GCS_CORS_HEADERS` | Unsupported global setting. JSON preflight already echoes every syntactically valid requested header, so JSON-only callers can remove it. For XML/public URLs, configure each bucket's CORS `responseHeader` (e.g. `["X-Example"]`) with the required origins/methods; there is no global native replacement | Diagnostics |
| `FAKE_GCS_LOCATION` | Unsupported global default today; future native-profile `GCS_MIMIC_DEFAULT_LOCATION=EU`. Legacy regional defaults remain unsupported; do not silently map `US-CENTRAL1` to `US` | `config-remaining-settings`; BEHAVIOR-BUCKET-003 |
| `FAKE_GCS_LOG_LEVEL` | Unsupported today; future `GCS_MIMIC_LOG_LEVEL=info` after structured events have levels | `config-log-level` |
| `FAKE_GCS_EVENT_PUBSUB_PROJECT_ID` | Unsupported publisher; remove this key for current startup. Real project integration needs separate EXT-BEH-032 delivery | Separate event plan |
| `FAKE_GCS_EVENT_PUBSUB_TOPIC` | Unsupported publisher; remove this key; do not suggest an ineffective native topic | Separate event plan |
| `FAKE_GCS_EVENT_BUCKET` | Unsupported publisher filter; remove this key; no independent bucket policy is implied | Separate event plan |
| `FAKE_GCS_EVENT_OBJECT_PREFIX` | Unsupported publisher filter; remove this key; no native prefix alias | Separate event plan |
| `FAKE_GCS_EVENT_LIST` | Unsupported publisher events; remove this key; no accepted-but-ignored event list | Separate event plan |

Unknown `FAKE_GCS_*` variables produce warnings and do not prevent ready.
`GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE` and `GCS_MIMIC_TLS_DIR` are the complete
Compose-only interpolation names in the current files. Each is a server-input
error: remove it from the native process environment and keep it in the Compose
invocation. PORT controls host publication (use LISTEN for server binding), IMAGE
selects the image (no server equivalent), and TLS_DIR selects a mount (native
TLS_CERT/TLS_KEY take the two actual files). They are not aliases.

With no `seed_dir`, a nonempty `/data` produces a warning recommending `GCS_MIMIC_SEED_DIR=/data`
and explaining the one-time behavior. It must not import data implicitly or
prevent startup; inspection errors are warnings too, rather than being treated
as proof the directory is empty. Do not recursively scan `/data` just to warn.
Suppress this migration warning when `/data` and effective `data_dir` overlap:
canonicalize both and compare equality and component-prefix ancestry in both
directions, as `seed.rs` does, including symlink aliases. If a path does not yet
exist, resolve its nearest existing ancestor and append the unresolved components
before comparing; a failure to establish disjointness produces inspection advice,
never a seed recommendation that could overlap native storage. This is a
conservative exception to the plan's warning rule because recommending native
catalog data as seed causes a startup failure. On non-container hosts the same
bounded existence/overlap checks apply; an unrelated `/data` is only a warning.
These intentional changes land after 0.1.0 in a new minor release.

## Shared diagnostics contract

Configuration, Initializer, Sync and Import use the same logical record:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticSource {
    Cli,
    Environment,
    Config,
    Tls,
    Identity,
    Filesystem,
    Catalog,
    Initializer,
    Seed,
    Sync,
    Import,
    Listener,
    Runtime,
}

struct Diagnostic {
    source: DiagnosticSource,
    key: String,
    fix: String,
}
```

The enum's declaration order is the fixed sort order; its serialized names are
`cli`, `environment`, `config`, `tls`, `identity`, `filesystem`, `catalog`,
`initializer`, `seed`, `sync`, `import`, `listener`, `runtime`. Input validation
uses its originating input source. Material loading uses `tls`/`identity`,
`/data` inspection uses `filesystem`, catalog open/recovery/close uses `catalog`,
bind/serve uses `listener`, and runtime/signal creation and otherwise unclassified
top-level failures use `runtime`. Phase failures use `initializer`/`seed`/`sync`/`import`.
`key` retains the original option/environment
name or field path, such as `--scheme`, `FAKE_GCS_PORT` or
`buckets[2].location`. For a document syntax/read failure use its input selector
or `$`. When no supplied token identifies an operational failure, use the logical
resource key: `tls_cert`/`tls_key`, `identity_file`, `/data`, `data_dir`,
`listen`/`listen_http`, or `runtime`/`signal` for the corresponding source.
`fix` describes the failure and corrective action, including a concrete
safe example where a value is needed. Unsupported capabilities explicitly say
unsupported and give removal/alternative guidance; never suggest that a reserved
setting already works. Do not echo credentials, private keys, signed queries or
secret input values. Escape control characters through JSON serialization.

Fixes may echo only validated legacy host, port, scheme, supported location and
log-level values, and validated HTTP(S) endpoint URLs without userinfo, query or
fragment. All other values use safe placeholders/examples; key names and field
paths remain available for identification. Redact an entire credential-bearing
or signed/query-bearing URL, not just its password. Do not include raw parser or
I/O error text that can bypass this rule. Secrets in identity/TLS files must not
appear in any diagnostic or duplicate stderr line.

For combined advice use pinned legacy precedence (last CLI occurrence over
nonempty environment over defaults) solely to compute the fix, without accepting
any legacy input. Fill missing/empty HOST=`0.0.0.0`, PORT=`4443`,
SCHEME=`https`, PORT_HTTP=`8000`; bracket IPv6 hosts in socket examples. Invalid
components receive placeholders and their own correction instead of a fabricated
valid value. `FAKE_GCS_PORT=8080` yields `GCS_MIMIC_LISTEN=0.0.0.0:8080`;
HOST=`127.0.0.2` with PORT=`9000` yields `127.0.0.2:9000`, not the native default
`127.0.0.1:4443`. EXTERNAL_URL=`http://gcs:8080/proxy` yields that exact safe
API base. PUBLIC_HOST combines its supplied safe authority with effective
SCHEME (`both` uses HTTPS). Supported LOCATION and LOG_LEVEL suggestions use the
supplied supported value (`warning` normalizes to `warn`); unsupported values
receive removal/valid-choice advice. Never silently translate legacy
`US-CENTRAL1` to native `US`.

The shared renderer writes one newline-delimited JSON event per nonempty severity
**per batch** to
**stderr**, with all records of that severity in its `diagnostics` array:

```json
{"event":"diagnostics","level":"error","diagnostics":[{"source":"environment","key":"FAKE_GCS_HOST","fix":"Legacy input is unsupported; combine HOST and PORT as GCS_MIMIC_LISTEN=0.0.0.0:4443."},{"source":"environment","key":"FAKE_GCS_PORT","fix":"Legacy input is unsupported; combine HOST and PORT as GCS_MIMIC_LISTEN=0.0.0.0:4443."}]}
```

The human reading rule is **source/key → fix**, with one record per original
setting occurrence, even when two keys share a fix. JSON is the sole output format; no duplicate
prose lines or new output-format option. Warnings use `level:"warn"` and the
same schema. Errors and warnings are collected separately, ordered by the enum
order above, then original key by UTF-8 byte order; preserve encounter order for
repeated occurrences of one key.
Emit warnings before errors and return nonzero if any error exists. Validation
diagnostic batches remain visible at every log level; filtering applies to
ordinary runtime events, not the report of invalid input. Stdout `ready` / `stopped` lifecycle
events always remain machine-readable and unfiltered.

Validation returns **all independently detectable problems in the bounded
input**, not the first one. A malformed file prevents inspecting its fields but
does not suppress independent CLI/environment checks. Skip only checks requiring
an invalid prerequisite; do not invent cascading TLS/URL errors. A later valid
CLI value may replace a malformed lower-priority native value under existing
precedence. Duplicate flags and known legacy names remain errors regardless.
Configuration checks and independently loadable TLS/identity material form one
startup preflight batch. Attempt both material loads when their effective
configuration prerequisites are valid, even if the other fails; skip only a load
whose path/profile prerequisites are invalid. No catalog mutation, listener bind
or ready event occurs on preflight errors. `config-legacy-input-diagnostics`
owns collecting these load results and converting `main()`'s top-level error
handler to the renderer, replacing the duplicate `error: ...` prose path.
The existing config → TLS/identity → catalog → bind phase order is preserved.
Later operational failures emit a new batch for the failed phase using the same
schema; cleanup errors join that batch. They do not claim to roll back already
committed work. No free-form error line is emitted alongside JSON. Existing
non-error runtime stderr events are converted to leveled JSON by `config-log-level`.

Each producer must bound input and diagnostic retention: configuration accepts
at most 1024 supplied setting occurrences across CLI/environment/file, with
original keys at most 256 UTF-8 bytes; the file retains its 64 KiB cap. Oversized
keys/counts produce an input-limit diagnostic with a corrective action before
collecting more entries, never silent truncation or success. Other producers
use their own documented bounded file/page limits and emit complete diagnostic
batches per validated page, with warnings before errors within each page; future
page warnings need not precede earlier page errors. One bounded configuration
file is one batch, not one event for the process lifetime. A paged operation's
final result remains failure if any page failed, with no duplicate run-wide
replay. They must not collect a whole filesystem catalog merely to
print all errors. This permits aggregation without unbounded memory.

`config-legacy-input-diagnostics` creates `Diagnostic`, collection and the minimal
shared event renderer; `config-log-level` adds level filtering. Initializer and
Sync reuse them. Bounded file readers belong to `init-parser`; URL normalization
deduplication belongs to `config-public-origin`. This document creates no Rust
helper and does not take another slice's implementation ownership.

## Host matching: measured reference and normative rules

Native path-style XML keeps accepting arbitrary Host values; explicit DNS
suffix routing and the existing absolute-authority conflict rule remain as in
the [virtual-host contract](xml-virtual-host.md). Authentication/signature
verification, IAM/PAP and XML CORS are still required after routing. Legacy
matching is opt-in through `compat_profile=legacy-emulator` with an explicit
`public_base_url`; an unset public base preserves current routing. It must never
become a general strict-mode host restriction or a proxy trust policy.

The pinned upstream [router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go)
was executed as a native binary with Go 1.27.3 using
`scripts/compare-reference-emulator.sh`. The
[evidence and raw records](../evidence/configuration-host-matching.md) distinguish
selected buckets with object bodies rather than treating every HTTP 200 as a
Host match. Reference-only observations do not implement gcs-mimic routing.

| Dimension | Measured reference behavior |
| --- | --- |
| Authority | A DNS/IPv4 public host without `:` matches the exact host with absent or any request port. A configured `:80` or `:8443` requires that exact suffix; omission/different ports fall through. Bracketed IPv6 contains `:`, so `[::1]` matches only `[::1]`, and `[::1]:8443` only that exact authority. |
| DNS | Public matching is byte/case sensitive, with no trailing-dot removal. Lookalikes fall through. The virtual public-host prefix is one label: `probe-bucket.storage.example.invalid` selects `probe-bucket`; dotted/overlapping prefixes fall through to the entire hostname bucket. Absent configured port allows a request port, while explicit port must match. |
| Route selection | JSON `/storage/v1` routes win even on a public-derived bucket host. Public virtual and public path-style requests select their respective buckets; arbitrary-Host GET/HEAD/PUT fallback selects the whole hostname (excluding its request port), not the first path component. |
| Request form | Origin-form uses Host. Go's HTTP server uses absolute-form target authority instead of a conflicting Host header. Duplicate, malformed and missing HTTP/1.1 Host return transport 400 before routing. |
| Method/error | Known GET returns its fixture body; HEAD is bodyless 200. Unknown bucket/object GET returns 404 `Not Found`. Public/virtual bucket-root GET returns an XML list; arbitrary-host root returns 404. PUT with the upstream algorithm-selector query selects the same route and persists bytes; this does not test real signatures. |

Two details are source inferences, not measured distinctions: the pinned
router registers public virtual routes before public path-style routes, but
the probes cannot match both at once; its virtual DNS matcher is case sensitive,
but the mixed-case probe returns 404 under either interpretation because no
mixed-case bucket fixture can be distinguished portably. gcs-mimic's bucket
validator rejects that mixed-case selected name with XML 400 either way.

`config-public-origin` implements these rules for the explicit legacy opt-in,
with the following complete dispatch boundary:

1. Require one syntactically valid Host. Reject malformed/missing/duplicate Host
   with 400. Syntax means one parseable authority, no userinfo and a numeric
   port when present; do not apply configured DNS-domain length/label rules to
   incoming Host. Compose names such as `fake_gcs` remain usable for JSON and
   probes. XML fallback uses the existing legacy bucket-name validator, which
   accepts underscores: an absent `fake_gcs` bucket yields XML `NoSuchBucket`,
   and a created one is routable. Before routing, reject a conflicting absolute-form target authority
   with existing XML 400 `InvalidArgument` (unsigned/bearer) or 403
   `SignatureDoesNotMatch` (signed); HEAD errors are bodyless. Never replace the
   signed Host with the target authority. This deliberate difference from Go's
   target-wins behavior preserves the existing authority/signature invariant.
2. For DNS, classify the selected domain using longest-suffix selection across
   the domain union. An explicit domain selected over the public-derived domain
   keeps native XML-only virtual-host routing, including reserved paths such as
   `bucket.explicit.example/storage/v1/b` as object `storage/v1/b`. If the same
   domain is both explicit and public-derived, use the public-derived rules.
   For public-derived candidates and path-style requests only, dispatch existing
   JSON, upload/download JSON and emulator helper/probe namespaces before legacy
   XML selection, without requiring a public Host match. This choice preserves
   the existing explicit-domain extension while applying measured JSON precedence
   only to the public-derived/path-style surface. Do not expose upstream
   administrative routes.
3. For the public-derived domain, require a single nonempty
   bucket label followed by `.` and the exact normalized public hostname. If
   the effective public origin has a nondefault port, require it exactly;
   otherwise ignore a request port. Use the object-only path. Dotted prefixes
   under this public suffix use the fallback, as measured. For candidate
   classification only, lowercase the hostname and remove a trailing dot;
   a public-domain candidate that fails the exact byte/label/port match must not
   be retried through native matching for the same or a shorter suffix.
4. Otherwise compare the request authority to the normalized public authority
   using the measured rules in the table (including the IPv6 colon exception).
   On a match select bucket/object from the path; on a miss use the complete
   request hostname, excluding its port, as the fallback bucket and the entire
   path as the object. IPv6 misses do not select a valid bucket. Preserve bucket
   name validation and return the existing XML 400 for an invalid selected name;
   a valid but absent bucket/object returns existing XML 404
   `NoSuchBucket`/`NoSuchKey` respectively.
   This fallback also applies to the API base's authority: with API
   `http://localhost:4443` and public `http://storage.example.invalid`, XML
   `GET /b/o` at the API endpoint selects bucket `localhost`, object `b/o`,
   normally returning 404. Use the same API/public authority for SDK XML reads.
   This retains upstream matching rather than silently exempting the API host.
5. Route only the already implemented XML GET/HEAD/PUT surface. Valid supported
   operations retain their current success/status/header behavior; bucket-root,
   listing, DELETE and form-upload capabilities retain their existing 501
   boundaries. Run ordinary authentication, signature, IAM/PAP and CORS checks
   against the selected bucket, never treating a matching Host as authorization.

Two other deliberate local boundaries remain: IP public origins disable virtual
routing even though upstream accepts artificial `bucket.127.0.0.1` hosts; and
normalization drops scheme-default public ports so browser/helper canonical
Hosts remain usable. The raw upstream explicit-default-port behavior is recorded
separately. Native routing and unset-public-base legacy routing keep their
existing dotted-prefix, longest-suffix and reserved-namespace behavior unchanged.
The public-origin implementation compares its native/legacy wire results with
this recorded contract; it does not inherit the reference's missing signature
checks, plaintext errors or extra methods. See the
[implementation evidence](../evidence/configuration-public-origin.md) for
verified cases and remaining integration work. Parent DESIGN-055/UX-003 rows
remain PARTIAL.

## Acceptance and delivery ownership

### Public origin implementation acceptance

The `config-public-origin` slice implements DESIGN-055-PENDING,
DESIGN-055-LEGACY-HOST and UX-003-PUBLIC-ORIGIN without completing their parent
rows. Acceptance covers DNS/IPv4/IPv6 origin validation, root-only paths,
default-port removal and normalization idempotence through property tests;
CLI/environment/file precedence; a 32-domain union with public/explicit merging,
explicit duplicate rejection, longest suffixes and API-host conflicts; and IP
warnings with the fixed ready fields. Shared startup's `ReadyEvent` emits the
fields from `Config.public_base_url` and `Config::public_virtual_host_enabled`,
preserving startup ordering. DNS/IP/unset ready values, the complete ready key
set, and invalid-origin rejection before catalog creation have process tests.

Wire tests identify the selected resource for the measured legacy authority,
port, case, trailing-dot, single-label and hostname-fallback rules above. They
also cover native/unset-public behavior, explicit/public overlapping suffixes,
reserved namespaces, malformed/duplicate/missing Host, absolute-form conflicts,
GET/HEAD/PUT boundaries and selected-resource errors. Strict tests retain IAM,
PAP, final-host signatures and tampered-Host rejection. Helper process tests
exercise explicit files, no implicit file/environment reads, overridden invalid
file origins, flat value types/size/syntax, server-only/unknown-key isolation,
base overrides and host/peer/browser receiver choices. No catalog, TLS material
or registry is opened by a URL helper. Existing SDK/container evidence remains
separate from these native wire/process checks; no proxy-derived origin or
performance improvement is claimed.

### Legacy diagnostics implementation acceptance

The diagnostics slice verifies the existing `UX-003-LEGACY-DIAGNOSTICS`
contract with focused parser, configuration, renderer and process tests before
marking it PASS. Production parsing and diagnostic modules deny Clippy
`unwrap_used` and `expect_used`; assertion conveniences are allowed only inside
their test modules. Configuration reports retain the independently valid
effective settings for TLS/identity preflight even when another setting fails;
invalid prerequisites suppress only dependent checks. The public `Config::load`
returns a typed diagnostic collection on failure, and the CLI emits its warnings
before its errors without duplicating prose. No catalog or startup phase is
reordered. Resource throughput, RSS, CPU and latency are unmeasured in this slice.
OS environment keys are classified before decoding values: unknown legacy
values are never interpreted, and effective known/native encoding failures join
other independently detectable diagnostics. Invalid native syntax retains its
prerequisite state, rather than validating dependent fields against defaults.
Native option names require exactly two hyphens; legacy names permit exactly
one or two. TLS material loading does not depend on handshake-timeout validity.

These cases are recorded in the ledger before implementation. Contract review
checks the 19-row inventory, defaults/validation/precedence, shared diagnostic
examples and measured Host rules. It remains PENDING until an independent
review has no major findings; command success alone does not pass that review
or the runtime cases.

| Ledger case | Required verification | Implementing slice |
| --- | --- | --- |
| `DESIGN-055-CONFIG-CONTRACT`, `UX-003-CONFIG-CONTRACT` | Independent review with zero major findings across all contract sections; markdownlint, ledger validate/render/check and `just check`; retain PARTIAL parent states and existing implemented scopes | `config-contract` |
| `UX-003-LEGACY-DIAGNOSTICS` | All 19 env/CLI names, empty input, one/two hyphens × separate/equals values, no-subcommand Compose argv, mixed equals/separate tokens and native-name collisions; native value-position regressions `--object -data`, `--object -port`, `--object -x` in public/sign helpers; value-derived HOST/PORT/scheme/URL/location/log advice, URL/secret redaction; JSON CORS removal versus XML/public bucket `responseHeader` advice and unsupported global replacement; legacy PUBLIC_HOST advice for unequal API/public authorities and SDK XML reads; all three Compose-only variables and replaced CMD listener/data guidance; `/data` disjoint/overlapping/ancestor/descendant/symlink/missing-path/inspection-error cases; 1024/1025 occurrences, 256/257-byte keys, deterministic source/key/repeated-key order, warnings before errors, control-character escaping; independent malformed config/TLS/identity errors, paged batch behavior and operational-source JSON; nonzero/no ready/no data creation and no parser panic | `config-legacy-input-diagnostics` |
| `DESIGN-055-API-DEFAULT` | Port 0, IPv4/IPv6 wildcard and explicit binds, explicit base precedence, proxy prefix; HTTPS primary for `scheme=both`; Host/Forwarded/untrusted origins ignored; preflight collision validation using explicit base or substituted listen host, including numeric DNS suffixes, with no catalog access/bind/ready on failure; fixed ready keys; host-published Compose port and resumable Location traversal | `config-api-origin-default` |
| `DESIGN-055-PENDING` | DNS/IP public origins, 32-domain union/dedup/overflow/API conflict, normalization idempotence proptest, non-root path/userinfo/query/fragment rejection, default-port normalization; explicit-config helpers (syntax/size/value types/public base only), override/no implicit reads/no TLS/identity/catalog access; final-host signature/tampering, host/peer/browser receiver choices and fixed ready fields; proxy derivation remains separate | `config-public-origin`; DESIGN-056 separately |
| `DESIGN-055-LEGACY-HOST` | Implement against the recorded reference authority/DNS/route/request-form matrix; verify selected buckets, status and deliberate local boundaries including explicit overlapping domains and explicit-domain reserved paths (XML) versus public-derived/path-style JSON precedence, same-domain explicit/public deduplication, unequal API/public authorities with API-host XML fallback and equal-authority SDK XML reads, absolute conflict/signature rejection, existing XML errors/methods and IP virtual disabling; native/unset-public reserved-path and arbitrary-Host regressions and strict IAM/PAP isolation | `config-public-origin` |
| `UX-003-PUBLIC-ORIGIN` | Explicit public origin for host/peer/browser receiver choices, independent API/listener roles, DNS/IP domain behavior and diagnostics, explicit-config helper defaults/overrides, final-host signature/PAP isolation and fixed ready keys | `config-public-origin` |
| `UX-003-DUAL-LISTENER` | HTTP and HTTPS simultaneously, one advertised base, aggregate connection cap, second-bind failure cleanup, retained ready key plus both addresses, shutdown of both; reject equal nonzero endpoints including primary port 8000 colliding with the default secondary; reject listen_http unless scheme=both; both port-0 listeners bind independently and report distinct actual ports; existing TLS trust/handshake tests | `config-dual-listener` |
| `UX-003-LOG-LEVEL` | Shared stderr JSON/levels; real filtering for each accepted level, invalid-level failure, fatal diagnostics and stdout ready/stopped always visible | `config-log-level` |
| `UX-003-REMAINING-SETTINGS` | Native location absent/explicit/default/restart, invalid regions and explicit legacy-setting rejection; retain case-insensitive legacy `US-CENTRAL1` acceptance, including `us-central1`, without storing it | `config-remaining-settings` |
| `UX-003-MEMORY-EVENTS` | Effective bounded memory backend and event publisher/filter behavior with their own decomposed acceptance; no ignored selector counts as success | Separate DESIGN-077 / EXT-BEH-032 plans |

DESIGN-064 precedence, duplicate/unknown/size/bounds cases and DESIGN-065
HTTP/HTTPS/default/custom-trust/handshake cases stay unchanged and must keep
passing. No resource benchmark, container build, SDK execution, browser run or
real-cloud request is required for this documentation-only slice. Runtime
slices record their own applicable performance and recovery evidence.

### Contract verification record

Executed on 2026-10-24 with the pinned Rust **1.99.1** toolchain. Results below
refer to the final contract/ledger and the Host-script blob recorded in the raw
evidence; the final `just check` run follows the comparison-script changes.

| Check | Result |
| --- | --- |
| Independent contract review | PENDING; the author does not grant PASS. Both ledger contract-review cases await independent review with zero major findings |
| `npx --yes markdownlint-cli2 docs/en/slices/configuration-workflows.md docs/en/evidence/configuration-host-matching.md` | PASS, markdownlint-cli2 0.24.0, two files, zero issues; MD013 disabled for the tables/examples |
| `cargo xtask compatibility validate` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields |
| `cargo xtask compatibility render` | PASS; both English/Japanese views regenerated with no tracked changes, and SHA-256 hashes were identical after a second render |
| `cargo xtask compatibility check` | PASS; generated views match the ledger |
| `just check` | PASS, one final-source run after the Host-script changes: fmt, clippy with denied warnings, 665 Rust tests (zero failures/skips), ledger and shell/release-script checks |
| Pinned reference-only comparison script | PASS; all 189 Host records asserted across six public authorities and raw malformed/duplicate/missing Host; status/bucket/body projections matched the previous evidence, excluding volatile fields |
| `bash -n scripts/compare-reference-emulator.sh` and `shellcheck scripts/compare-reference-emulator.sh` | PASS; zero syntax/lint issues. The Host assertions also rejected deliberately changed statuses in 13 routing groups |
| Local-path scan and `git diff --check` | No new local paths or whitespace errors; the path-pattern text already in the planning document is the only tracked scan match |

The existing configuration precedence/bounds, CLI failure-before-ready, TLS
material/default/trust/admission, virtual-host and signed Host tests pass in
`just check`. No source capability or pending runtime acceptance is promoted.
The two contract-review cases are PENDING and the parents' `last_verified` values
remain at 2026-10-23; this run did not re-execute their historical SDK/container
journeys. No SDK/container/browser journey or performance measurement was made.

The required slice-start `cargo xtask dependencies check` exited **1** because
the runtime image has an available update (other checked inputs were CURRENT):

```text
docker distroless/cc-debian13:nonroot: UPDATE_AVAILABLE current=sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf latest=sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e
```

Distribution owns dependency freshness, including this release-blocking runtime
image update; this configuration contract does not duplicate its remediation
record or deadline. The existing pin is preserved, not declared current. Next
implementation can begin with `config-legacy-input-diagnostics` after independent
contract review; public-origin delivery uses the measured Host rules above.

### Legacy diagnostics runtime verification

The diagnostics implementation and focused regression tests are recorded in
[configuration diagnostics evidence](../evidence/configuration-diagnostics.md).
It delivers the 19-name migration inventory, bounded aggregate configuration
validation, independent TLS/identity preflight and the shared JSON renderer.
The earlier contract-only verification record is historical; it does not
describe newly reserved settings as implemented. Endpoint-default resolution,
public-origin routing, dual listeners and log-level filtering remain the named
future slices. Literal host `/data` is not modified by tests; isolated paths
exercise the production inspector. Runtime/signal allocation and simultaneous
cleanup failures are not fault-injected, and performance remains unmeasured.
