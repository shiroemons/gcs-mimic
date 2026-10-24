# Public origin configuration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified scope: DESIGN-055-PENDING, DESIGN-055-LEGACY-HOST and
UX-003-PUBLIC-ORIGIN, against the
[configuration contract](../slices/configuration-workflows.md). Their parent
DESIGN-055 and UX-003 rows remain PARTIAL. The existing
[pinned reference Host matrix](configuration-host-matching.md) is comparison
evidence; native Rust wire tests establish this implementation's behavior.

## Implemented behavior

- One shared HTTP(S) base validator serves API configuration and URL helpers;
  public origins additionally require a root-only path, validate DNS/IP hosts,
  lowercase DNS, canonicalize IPv6 and omit scheme-default ports. The validated
  `PublicOrigin` retains private typed host data and a parsed authority; routing
  never re-parses the configured origin or silently disables matching. Property
  tests generate multi-label mixed-case DNS, random IPv4 and uncompressed IPv6,
  absent/80/443/random nonzero ports and trailing slashes. Port zero is tested
  separately as rejected input. Arbitrary Unicode strings remain covered for
  panic safety and accepted-result idempotence. Accepted origins are
  idempotent and usable by XML selection and the signing helper. Base prefixes
  are also checked through structured inputs.
- CLI/environment/file precedence, explicit-list duplicate rejection and a
  32-domain union are enforced before startup. A public DNS domain already in
  the list is merged; IPs produce a warning and disable only the derived
  virtual-host effect. API/virtual-bucket conflicts remain rejected.
- Native path-style and legacy mode without a public origin keep accepting
  arbitrary Host values. Explicit legacy public routing implements case/port/
  trailing-dot/single-label matching and complete-hostname fallback, preserves
  explicit-domain XML routing, and gives JSON/probes precedence on public-derived
  and path-style surfaces. Tests identify selected buckets using object bytes.
  Invalid selected buckets, missing resources and unsupported XML methods retain
  their existing XML errors.
- Missing/malformed/duplicate Host is rejected for the legacy opt-in; conflicting
  absolute-form authority is rejected with unsigned 400 or signed 403, with
  bodyless HEAD. Strict bearer permissions, signed GET/PUT, raw Host/port binding
  and PAP remain enforced. Host selection never grants access.
  Incoming Host uses syntax checks, so underscored Compose names reach JSON and
  probes. Their XML fallback preserves legacy bucket-name rules and selects the
  complete hostname. The bucket/raw path selection retains its URI and is
  computed once for CORS and dispatch; callers cannot substitute another URI
  when extracting the object name. Public path, public virtual and hostname-fallback tests
  verify allowed/disallowed CORS origins, preflight, real reads and authorization
  errors against the selected bucket.
- Both offline helpers read only explicit `--config PATH`, validate size/syntax/
  flat types and public origin even when overridden, and ignore server-only
  semantics/unknown keys. They never read implicit configuration/environment,
  server TLS/identity material, catalogs or the network. `--base-url` wins.
  File-read errors preserve the selected server CLI/environment source; helper
  file errors use structured `--config` keys and field errors use the original
  JSON key. Flat-value advice comes from one shared definition.
  Host, peer and browser bases are exercised as independent receiver choices;
  explicit-config signing is cryptographically verified at its final host.
  Native process helpers also generate public/signed URLs used in real local
  HTTP requests with each receiver's final Host. Equal API/public authorities
  preserve the SDK-style XML path read; different authorities retain the tested
  complete-host fallback. This wire test does not claim a new official SDK run.

## Verification

Official Google sources were re-fetched on **2026-10-24**; SHA-256 of the exact
HTML responses pins the source content used for this review:

| Source | SHA-256 |
| --- | --- |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `bba83ed5e93236f73b69c8ec31e2ede0122e5274877e6151fdfbddb0efaa8704` |
| [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints) | `3a94e04d17ec057d28bda6231b30b33a57cf4074ad401fbb4c8b1fa0b2a8baf9` |

The public origin is emulator configuration, not a new Google request field.
The existing gaps were reviewed and remain open; final Host/path/header signing
and XML endpoint selection follow these official contracts.
The WHATWG URL Standard's
[numeric-host checker](https://url.spec.whatwg.org/commit-snapshots/845bafca22fb0c5a42c4bf384a9bdea525a2f10d/#concept-ends-in-a-number)
was checked on the same date at revision
`845bafca22fb0c5a42c4bf384a9bdea525a2f10d`. Noncanonical numeric hosts are rejected
with a canonical IPv4 example, avoiding browser conversion into an unintended
authority/domain. Canonical IPv4 keeps the specified warning and virtual-disable
behavior. Existing explicit-domain validation is unchanged.

Focused commands executed with the pinned Rust 1.99.1 and locked dependencies:

```sh
cargo test --locked --offline --lib -- config:: url:: config_input::
cargo test --locked --offline --test public_origin_cli --test url_cli --test sign_url_cli
cargo test --locked --offline --test auth_http virtual_host
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

Before readiness integration, the focused results were **34**, **14** and
**15** passing tests respectively; the final workspace run includes **17**
virtual-host tests after the two readiness/startup additions. Clippy passes with
warnings denied. The existing legacy-advice test
also passes after replacing future public-origin advice with the now effective
native setting and explicit legacy profile.

After shared startup integration, process tests verify normalized DNS/IPv4/IPv6
and unset ready values (including null key presence), nondefault port retention,
IP warnings from the same startup, unchanged configured API links, and invalid
origins/domain overflow/API-host conflicts before catalog creation or ready.
The existing shutdown/restart test
checks the complete ready key set. These **3 targeted tests** pass. The final
source `just check` passes with **721 Rust tests**, reported in **37 result
groups** (33 nonempty), including the ready and rejection cases. The
[single final check log](configuration-public-origin-checks.txt) records fmt,
Clippy with warnings denied, all workspace tests, ledger consistency and shell
checks. Local path prefixes are redacted. The final source is commit `74d02a4`,
based on shared startup `8d2cb71`; later changes in this slice update only
documentation/evidence and the ledger.

An intermediate full `cargo test --workspace --locked --offline` run failed
`final_hash_scan_uses_request_budget_instead_of_catalog_wait_budget` with
`left: 503`, `right: 200` (8 MiB scan/publication: 258.814 ms, catalog wait
budget: 50 ms). The exact isolated test then passed in 1.39 s, reporting a
149.906708 ms scan/publication. The assertion and implementation were preserved;
this observation does not establish a root cause. The final unmodified standard
`just check` retains this test and passes it; no assertion was weakened or skipped.

The slice-start `cargo xtask dependencies check` found the compiler and direct
Rust/SDK inputs current, with the already recorded distroless runtime digest
update still available. No dependency was added or changed. Distribution owns
that existing update; see the [dependency check](configuration-public-origin-dependencies.txt).

## Limits

No container build, SDK suite, browser run or real-cloud request has been run for
this slice. Existing container/SDK evidence remains separate. No payload,
catalog or durability changes; no benchmark result is
claimed. Trusted-proxy origins remain DESIGN-056. Existing SPEC-XML-VIRTUAL-HOST
and SPEC-SIGNING-EDGES remain OPEN; legacy matching is a local comparison profile,
not a Google API fidelity promotion.
