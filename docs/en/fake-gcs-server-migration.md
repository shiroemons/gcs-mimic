# fake-gcs-server migration and compatibility target

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

## Goal and current state

The migration goal is to preserve existing application workflows while adding
stricter Google-contract validation, broader capabilities and resource controls.
CLI spelling, configuration structure and internal storage need not be identical
to fake-gcs-server. Document equivalent uses and explicit differences; the broader
full-superset gate below tracks remaining functional coverage.
This is a target, not a current compatibility claim. Migration importers,
Go embedding and source-only startup settings remain unavailable.
The existing `legacy-emulator` profile covers a limited bucket contract; it does
not implement the complete migration profile described here.

gcs-mimic serves path-style and opt-in virtual-host XML GET/HEAD/PUT, strict
V4 RSA signed transfers and scoped public access prevention, with
[recorded evidence](evidence/signed-urls.md). URL construction and authorization
remain separate. Broader XML/listing, ACLs, UBLA transitions, permissive legacy
signing and complete endpoint/configuration equivalence remain gaps.

The first reference is the fake-gcs-server release in the
[detailed comparison](feature-comparison.md).
Record its complete surface before implementation and add later releases as
explicit baselines. Do not present one release's passing tests as proof about
all versions. No baseline
capability is excluded from the full-superset target merely because it is absent
from the current GCS inventory or the early-release queue.

The ledger's `delivery_plan.migration` owns the planning scope and acceptance
milestones. Existing feature statuses and evidence remain authoritative; this
document adds no passing migration result.

## Bucket core baseline before further bucket extensions

The create-by-name, basic-resource GET and empty-bucket DELETE baseline is
delivered on **2026-10-22** with source-supported versioning/default hold
configuration and real object effects. Select `compat_profile=legacy-emulator`
for source-specific naming/defaults/input/deletion behavior; native and strict
authorization retain their independent contracts. This is a named capability
baseline, not complete migration or full Google-method compatibility.

The [final integrated evidence](evidence/bucket-core-parity.md) records Rust,
pinned Go, both Linux architectures and the full runnable guide.
The [executed comparison](evidence/bucket-core-comparison.md) and
[raw records](evidence/bucket-core-comparison.txt) contain 92 requests each
against filesystem and memory reference backends plus both local profiles,
with no skipped profile. The reference is release `v1.56.1`, commit
`896ece51e49eed0946089f3089f6343855c0d106`.

| Operation | Executed result | Remaining difference |
| --- | --- | --- |
| Create by name | Source-valid names, omitted project, duplicates and failed-state preservation pass; both settings persist and affect HTTP object mutations | Native requires project/Google-oriented names. Profile retains strict project ownership, input bounds and 501 for unsupported protection; fs reference rejects versioning true |
| Basic resource | Create/GET match in every target; profile emits both booleans and source US-CENTRAL1/project/ETag defaults; list items match profile GET; restart/migration and strict permissions pass | Millisecond timestamps, local parser messages, native list scoping/paging, 666-byte profile name cap and unsupported trailing collection slash |
| Empty deletion | Profile returns exact 200 JSON null/newline and nonempty 412; native returns 204/409. Repeat/missing, recreate, protected histories and preserved object bytes pass | Atomic/history-aware delete and enforced holds intentionally exceed source live-only/no-hold behavior |
| Versioning/default holds | HTTP API creation enables retained versions and inherited holds in both profiles; selected history read/list/delete and hold release pass, with pinned Go workflows | Propagation/provider edge parity, temporary holds, retention and soft delete remain pending; supported PATCH/PUT settings are now delivered |

Source identities inspected at that revision are unchanged:

| Source | Git blob |
| --- | --- |
| `fakestorage/bucket.go` | `b1cfdbb9e6100a917888e309660cadf11a2de657` |
| `fakestorage/response.go` | `1a19a7fafa44bf2f80899c9b1eb70bdc6a75f2ed` |
| `fakestorage/json_response.go` | `7a10f7160d86fc154d904dc3ca3bd286eafcdaab` |
| `fakestorage/bucket_test.go` | `517271a4cebf115af54a94211ddedb08b38fef23` |

Every observed difference is classified as intentional, strict or unsupported
in the [scope contract](slices/bucket-core-parity.md#state-and-remaining-boundaries)
and comparison report, including object-resource framing/ACL/identity differences
on preservation probes. Those probes verify exact retained bytes, not complete
source object wire rendering. Official Google documentation, Discovery and
googleapis definitions remain the contract authority.

The three broad JSON bucket rows remain PARTIAL with their original policy,
ACL/owner and advanced cases. Versioning/default-hold behavior rows also remain
PARTIAL for unresolved provider interactions. Supported setting updates are now
delivered through [PATCH](evidence/bucket-metadata-patch.md) and
[PUT](evidence/bucket-metadata-put.md); this does not complete the broad methods.
All other source surfaces, configuration flags, helpers/events, Go embedding,
legacy gRPC and lossless import/export remain required by the full-superset
gate below; this delivery does not remove them.

## Compatibility surfaces

The observations below come from fake-gcs-server's README, configuration,
server implementation and gRPC adapter.
Inventory every flag, exported Go API and registered route before freezing the
executable contract; these examples are not the entire inventory.

| Surface | Observed source behavior | Required migration result |
| --- | --- | --- |
| Startup | Flags and `FAKE_GCS_*` environment variables; flags take precedence | Provide documented equivalent uses with native `GCS_MIMIC_*` settings; identical option names are not required |
| Listener/TLS | HTTPS by default; HTTP and dual-listener modes, certificate settings | Existing scheme/ports/trust configuration continue to work |
| Endpoints | Public/virtual-host and API routes, external upload URLs and mutable internal configuration | Preserve client-visible host/path/query behavior; map advertised addresses explicitly |
| HTTP semantics | JSON/XML, uploads, ranges, conditions, metadata, ACLs, batching and error shapes | Run the same client scenarios and negative cases against both implementations |
| Permissive signed URLs | Signature/expiry are not validated; callers can rewrite hosts | Preserve tested legacy behavior only in an explicitly selected compatibility profile |
| Fixture preload | Docker mounts `/data`; the entrypoint passes `-data /data` | Import the seed format and reproduce the selected seed/reseed lifecycle |
| Durable data | Filesystem root with object files and xattr/sidecar metadata | Read-only, verified conversion into a separate destination |
| Memory backend | In-process bytes and active/archived versions | Optional ephemeral behavior and export before the source process stops |
| Go embedding | Constructors, server lifecycle, clients/handlers and direct object methods | A separately tested Go-compatible adapter; a Rust executable alone is insufficient |
| gRPC | Source imports Storage **v1** protobufs | Test that legacy v1 surface separately; Storage v2 support cannot substitute for it |
| Helpers/events | Internal config/reseed routes and notification/event settings | Inventory and test local behavior plus explicit external integration boundaries |

Application source should not need modification. Renaming environment keys,
changing an image/binary or a Go module replacement and performing an explicit
data conversion are deployment steps, which must be documented and tested.
Do not claim literal in-process Go library replacement merely because equivalent
HTTP operations exist.

## Startup options and current equivalents

The [complete environment reference](fake-gcs-server-configuration.md) lists all
19 source variables, defaults, CLI precedence and current native mappings.
Source variables are not accepted aliases in the current gcs-mimic binary.

These are role mappings, not accepted aliases. gcs-mimic uses `serve --name VALUE`
and native `GCS_MIMIC_*` environment variables. Do not copy a source command and
assume that changing the image name is sufficient.

| fake-gcs-server option | Current gcs-mimic setting | Difference |
| --- | --- | --- |
| `-scheme http` / `https` | `--scheme`, `GCS_MIMIC_SCHEME` | One listener; `both` is unavailable |
| `-host`, `-port` | `--listen IP:PORT`, `GCS_MIMIC_LISTEN` | Combined listener setting; container port publication remains separate |
| `-external-url` | `--api-base-url`, `GCS_MIMIC_API_BASE_URL` | Fixed origin for API-generated links; not a public-host allowlist |
| `-public-host` | `--public-base-url`, `GCS_MIMIC_PUBLIC_BASE_URL` | HTTP(S) origin; adds a DNS virtual domain. Legacy Host matching requires `--compat-profile legacy-emulator`; native path-style accepts any Host. |
| `-data` | `--seed-dir`, `GCS_MIMIC_SEED_DIR` | Initial import into a fresh catalog, not startup replay or live synchronization |
| `-filesystem-root` | `--data-dir`, `GCS_MIMIC_DATA_DIR` | Persistent destination with a different format; cannot reuse the source root directly |
| `-cert-location`, `-private-key-location` | `--tls-cert`, `--tls-key` | Both PEM paths are required for custom TLS |
| `-cors-headers` | Bucket `cors` through the JSON API | Source setting extends globally allowed request headers; gcs-mimic XML rules are per bucket, JSON defaults are fixed |
| `-backend memory` | None | Current backend is persistent SQLite metadata plus files |

The source's `-public-host` controls public URL construction and Host matching
on relevant public routes; it does not select the listener. A configured port
participates in its matching. `-external-url` separately controls API-generated
URLs. Native virtual-host domains are DNS suffixes without a port; the actual
request authority still includes the port for strict signature checks.
Neither product's public-host/URL setting grants bucket permissions.
Public-origin normalization removes HTTP 80/HTTPS 443 and lowercases DNS;
non-default ports remain significant. IPs support path-style only. With legacy
matching, use equal API/public authorities for SDK XML reads: a different API
authority falls back to a hostname bucket. URL helpers use the file's
`public_base_url` only when `--config PATH` is explicit;
`--base-url` overrides it.

For example, native `url public --base-url http://localhost:4443 --bucket assets
--object hello.txt` generates a path-style URL. For `assets.storage.test`, enable
`--xml-virtual-host-domains storage.test` and arrange client DNS. HTTPS also needs
a matching certificate. Neither action changes IAM or makes a private object
public. Sign the final recipient-visible URL; strict signatures cannot survive
arbitrary host rewrites.

Source: pinned [configuration](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go)
and [routes and URL helpers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go).
The table is not a complete flag inventory; event, logging, dual-listener and
other source-only settings still need individually scoped decisions.

Use native names where they already express the intended role. Retaining every
source suffix or adding aliases is not required. Keep CLI > environment > server
JSON > default precedence. If aliases are added later, reject contradictory
values at the same precedence rather than depending on iteration order.
Legacy `FAKE_GCS_*` keys do not select compatibility mode. The development
binary rejects the 19 known keys with migration advice and warns on unknown
names; these are diagnostics, not aliases. See the
[migration diagnostics](fake-gcs-server-configuration.md#migration-diagnostics).

`GCS_MIMIC_PORT` in the repository's source-build Compose file controls host
publication only. It is not a native server option. Seed input, durable storage,
listener addresses and advertised URLs must retain their distinct meanings.

## Configuration compatibility and extensions

Keep server-wide settings in CLI options, environment variables or the existing
server JSON file. Store CORS, IAM, labels and supported versioning settings per
bucket through the shared bucket API and catalog. Do not create per-bucket
environment-variable suffixes or overload a global public-host setting with
bucket permissions or CORS.

The agreed extension is an initialization file listing multiple buckets and
their individual settings, with later API updates. On startup, create only
missing buckets. Existing bucket settings and objects survive unchanged, even
when the file differs; removing an entry does not delete its bucket. This keeps
restart from undoing API changes. Changing an existing bucket remains an API
operation. This initializer is not implemented; the file syntax and CLI entry
point are still to be designed.

The initializer must use the same field validation, authorization, persistence
and unsupported-setting errors as the bucket API. Validate the whole file before
mutations, reject duplicate names, and verify retry behavior after interruption.
Two buckets with different CORS/versioning rules must stay independent after
API changes and restart. Do not claim that accepting a field implements its
behavior. The planning record is
`delivery_plan.migration.configuration_contract` in the feature ledger.

## Profiles and conflicting behavior

Keep Google-contract/strict behavior and legacy emulator compatibility distinct.
The accepted option `--compat-profile legacy-emulator` implements the limited
bucket baseline above. The broader `fake-gcs-server compatibility profile`
remains a design target; freeze additional behavior with each affected slice.
The existing `auth_mode=permissive` is not already a complete legacy profile.

- A compatibility corpus reproduces the pinned emulator's observable contract,
  including its tested permissive URL, metadata and error behavior.
- A strict corpus checks local identities, effective permissions and valid
  signatures against the Google contract. Legacy signature acceptance must not
  leak into strict requests.
- Existing native seeding remains one-time and does not replay over mutations.
  Source-compatible seed/reseed behavior must be selected explicitly; durable
  migration must never silently replay fixtures over imported changes.
- Resource limits and unsupported settings remain visible. A recognized setting
  must not be accepted with no effect merely to pass a startup test.

Record contract conflicts and their profile-specific expectations. Passing a
legacy case is not Google `EXACT` fidelity. New strict restrictions cannot be
silently imposed on applications selecting the legacy contract. An unresolved
required conflict blocks the full compatibility claim.

## Editable directory requirement

Replacement also requires a visible bucket/object directory with bidirectional
synchronization: external additions, changes and deletions reach the API, and API
mutations appear in that directory. This is not implemented. Native seed import
and the planned one-time source converter do not satisfy this requirement.

SQLite may remain the metadata/index catalog. A synchronized working tree can
feed the existing bounded publication engine while preserving immutable internal
blobs. Conflict behavior, synchronization delay and unusual-name representation
must be settled before implementation. The planning contract is
`delivery_plan.migration.filesystem_sync_contract` in the feature ledger.
See the [source investigation and executed file-edit cases](fake-gcs-server-storage.md).
Safe reconciliation must update hashes and generations; copying the source's
stale-metadata behavior is not a sufficient editing contract.

## Data movement without coupling internal formats

Support source readers that produce typed bucket/object/version records and
bounded body streams for the shared catalog/blob import path. Keep source
parsing outside JSON/XML handlers and catalog internals. Do not add a generic
storage-engine abstraction just to read a foreign format.

Three source formats must remain distinct:

1. **Seed directory:** the source loader treats regular files below bucket
   directories as object content and derives MIME types from extensions. Native
   gcs-mimic seed input currently uses octet-stream. Do not claim the existing
   seed implementation already preserves fake-gcs-server metadata/defaults.
2. **Persistent filesystem:** decode the source's bucket metadata, object
   attributes and xattr/sidecar selection from the selected source format.
   A sidecar is metadata here; do not reuse the seed loader's regular-file rules.
3. **Live memory/API export:** quiesce source writes while it stays alive, export
   live and archived generations using supported APIs or the Go adapter, then
   import the bounded manifest/body stream. Stopping first loses memory state.

The destination is always a different data directory. Mount offline source
snapshots read-only, preserve xattrs in backups when they are used, and never let
old and new servers write the same storage root. A copied directory without its
xattrs is not automatically a complete export. Internal filesystem layout
compatibility is not required to preserve application behavior.

### Preflight and preservation requirements

- Record source version/backend, configuration, selected profile, stable export
  identity and explicit project/identity mappings. Do not invent missing project
  ownership or silently make a private resource public.
- Dry-run the full manifest and report unsupported metadata, malformed records,
  missing attributes, name/path collisions and corrupt/missing bodies. Do not
  skip failures or report partial copy as successful migration.
- Preserve raw logical names, bytes, MIME/content headers, custom metadata,
  bucket settings and ACL/retention/version state that the source exposes.
  Preserve source-visible generation, timestamp and ETag semantics: assigning
  fresh values must not silently invalidate stored generation URLs or conditions.
- Store imported identity separately from opaque blob paths, and advance future
  generation allocation so imported identities cannot be reused. If the current
  representation cannot retain a value or its precision, block and report it.
- Stream and independently verify lengths and checksums. Report inconsistent
  source hashes instead of silently replacing metadata or bypassing validation.
- Persist an import journal with bounded cursors and source fingerprints. Retry
  only the same export; never overwrite unrelated destination objects. Changed
  inputs, conflicts, disk-full and interruption require explicit outcomes.
- Publish the imported dataset only after validation and readiness checks.
  Reuse the destination's existing blob/catalog recovery rules; import itself
  does not make a cross-file transaction atomic.

Unsupported advanced attributes or archived versions stay blocking findings
until their semantics exist. An early live-object-only importer can have its
own declared scope, but it cannot be called lossless full migration.

### Cutover and rollback

Quiesce mutations, complete or cancel uploads and export a stable source. Do not
assume old resumable session URLs or page tokens will survive replacement.
Either preserve and test a selected token contract or require a documented drain
and restart boundary before cutover. Count resources/generations and compare
bytes, hashes, metadata, permissions and old generation-qualified URLs.

Switch the endpoint only after verification. Retain the original snapshot and
configuration, and test reopening/restoring them. Switching back to an old
snapshot after new writes would lose those writes; rollback must export/replay
the changes or remain inside an explicitly write-frozen verification window.
Do not automatically delete the source or claim bidirectional live migration.

## Delivery and full-superset gate

The migration track complements the capability queue; it does not mark an entire
phase complete or bundle every missing feature into one implementation turn.

| Milestone | Completion evidence |
| --- | --- |
| Pinned compatibility baseline | Versioned route/flag/Go/backend inventories and reproducible positive/negative corpus with every known gap visible |
| Seed compatibility | Same fixtures produce the required keys/bytes/defaults and selected lifecycle in both profiles |
| Persistent import | xattr and sidecar fixtures, identity/metadata preservation, dry-run, restart/resume and nondestructive rollback cases |
| Go and live-state migration | Export before shutdown, archived-version preservation and unchanged representative Go tests through the adapter |
| Full replacement validation | Every baseline surface passes, including TLS, legacy gRPC, helpers/events and memory behavior; remaining exclusions prevent the full claim |

Normalize only documented nondeterministic fields, such as a chosen ephemeral
port or an unconstrained newly generated request identifier. Do not normalize
away missing fields, errors, permissions, imported generations or wrong bytes.
Run both source backends, relevant official SDKs and representative existing
application tests. Review applicable upstream tests with their licenses intact.

A partial developer release remains possible and must identify its supported
migration scope. The phrase "fully backward-compatible superset" becomes a
release claim only after the complete pinned baseline and preserved-data checks
pass. Additional APIs, strict authorization and resource/performance results are
reported separately with their own evidence. New upstream releases expand the
baseline explicitly rather than silently changing the denominator.

Product **1.0.0** additionally requires the fixed extension scopes and stable
public contract in the [release policy](release-policy.md#the-100-gate).
Earlier useful increments ship as `0.x` releases without a full-superset claim.

## Sources consulted

Reviewed on **2026-10-22**. Source inspection does not mean the source tests,
importer or compatibility corpus ran.

| Source | Purpose |
| --- | --- |
| fake-gcs-server README and Dockerfile | Startup, URL behavior and fixture entrypoint |
| `internal/config/config.go` | Flags and environment settings |
| `fakestorage/server.go` | Seed loading, routes, constructors and lifecycle |
| `fakestorage/config.go` | Mutable internal configuration |
| `fakestorage/object.go` | Direct Go object APIs |
| `internal/backend/object.go` | Persisted attributes and identities |
| `internal/grpc/server.go` | Legacy Storage v1 contract |
| `docs/en/storage-backends.md` | Persistence alternatives and recovery tradeoffs |
