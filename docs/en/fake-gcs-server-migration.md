# fake-gcs-server migration and compatibility target

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

## Goal and current state

The product goal is a complete behavioral superset of fake-gcs-server after the
documented environment-name change and data conversion. Existing applications
and tests should keep working while gcs-mimic adds stricter
Google-contract validation, broader capabilities and measured resource controls.
This is a target, not a current compatibility claim. Migration importers,
equivalent renamed settings, a Go compatibility package and the compatibility
profile described here are not implemented.

Native gcs-mimic now serves the [path-style XML object GET/HEAD subset](slices/public-object-reads.md),
including public `allUsers` and registered-only `allAuthenticatedUsers` bucket
grants in strict mode. Its URL helper only constructs an encoded address;
the policy determines access. This does not yet replace fake-gcs-server's
virtual-host routing, broader XML reads/writes/listing, permissive signed URLs
or full public-host configuration. ACL and UBLA/PAP state machines remain
unavailable; do not infer migration parity from a successful public download.

The first reference is the fake-gcs-server release in the README comparison.
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
| Versioning/default holds | HTTP API creation enables retained versions and inherited holds in both profiles; selected history read/list/delete and hold release pass, with pinned Go workflows | Bucket settings PATCH/PUT, propagation/provider edge parity, temporary holds, retention and soft delete remain pending |

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
PARTIAL because changing bucket settings and unresolved provider interactions are
still open. The next slice is bucket metadata PATCH for labels/default class.
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
| Startup | Flags and `FAKE_GCS_*` environment variables; flags take precedence | Rename the prefix to `GCS_MIMIC_*`; preserve setting semantics and CLI precedence without reading legacy keys |
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

### Environment-name migration

Use `GCS_MIMIC_` in place of `FAKE_GCS_`, retaining the suffix, value meaning and
CLI-over-environment precedence for the migration settings. Do not accept
`FAKE_GCS_*` as aliases, fallback values or a trigger for compatibility mode.
If supplied, fail with a key-renaming diagnostic rather than silently applying
the old value or starting with unexpected defaults. These mappings are planned;
the current binary does not implement the complete settings below.

| Source key | Target key | Preserved meaning |
| --- | --- | --- |
| `FAKE_GCS_SCHEME` | `GCS_MIMIC_SCHEME` | HTTP, HTTPS or both in the selected compatibility profile |
| `FAKE_GCS_HOST` | `GCS_MIMIC_HOST` | Listener host |
| `FAKE_GCS_PORT` | `GCS_MIMIC_PORT` | Server listener port in direct-server configuration |
| `FAKE_GCS_PORT_HTTP` | `GCS_MIMIC_PORT_HTTP` | HTTP listener port in dual-listener mode |
| `FAKE_GCS_DATA` | `GCS_MIMIC_DATA` | Seed input, equivalent to native `GCS_MIMIC_SEED_DIR` when its format/profile matches |
| `FAKE_GCS_FILESYSTEM_ROOT` | `GCS_MIMIC_FILESYSTEM_ROOT` | Persistent storage location, with explicit conversion for an existing legacy root |
| `FAKE_GCS_BACKEND` | `GCS_MIMIC_BACKEND` | Persistent versus ephemeral backend behavior |
| `FAKE_GCS_EXTERNAL_URL` | `GCS_MIMIC_EXTERNAL_URL` | Advertised external API/upload endpoint |
| `FAKE_GCS_PUBLIC_HOST` | `GCS_MIMIC_PUBLIC_HOST` | Accepted public host/port |
| `FAKE_GCS_CORS_HEADERS` | `GCS_MIMIC_CORS_HEADERS` | Source-compatible response-header configuration |
| `FAKE_GCS_EVENT_PUBSUB_PROJECT_ID` | `GCS_MIMIC_EVENT_PUBSUB_PROJECT_ID` | Event project |
| `FAKE_GCS_EVENT_PUBSUB_TOPIC` | `GCS_MIMIC_EVENT_PUBSUB_TOPIC` | Event topic |
| `FAKE_GCS_EVENT_BUCKET` | `GCS_MIMIC_EVENT_BUCKET` | Event bucket filter |
| `FAKE_GCS_EVENT_OBJECT_PREFIX` | `GCS_MIMIC_EVENT_OBJECT_PREFIX` | Event object-prefix filter |
| `FAKE_GCS_EVENT_LIST` | `GCS_MIMIC_EVENT_LIST` | Selected event types |
| `FAKE_GCS_LOCATION` | `GCS_MIMIC_LOCATION` | Default location |
| `FAKE_GCS_CERT_LOCATION` | `GCS_MIMIC_CERT_LOCATION` | TLS certificate input |
| `FAKE_GCS_PRIVATE_KEY_LOCATION` | `GCS_MIMIC_PRIVATE_KEY_LOCATION` | TLS private-key input |
| `FAKE_GCS_LOG_LEVEL` | `GCS_MIMIC_LOG_LEVEL` | Log level |

Verify this inventory against the source before implementing it; later releases
may add settings.
Do not map seed `DATA` to native `DATA_DIR`: the latter is durable destination
storage. Existing `LISTEN`, `SEED_DIR`, `DATA_DIR` and `API_BASE_URL` retain their
native meanings. Specify field-level conflict handling when a renamed setting
and its native equivalent are both supplied: CLI wins over environment, while
contradictory values at the same precedence level fail instead of depending on
map iteration order. Renaming must not enable unsupported semantics silently.

`GCS_MIMIC_PORT` already controls the host-published port in `compose.yaml`;
that Compose variable is not currently forwarded as a server listener setting.
Keep host port publication and the container listener distinct when adding the
renamed settings, including a published port of zero. Verify direct-server and
Compose use independently. The compatibility profile supplies source defaults;
it must not silently change the current native profile's defaults.

## Profiles and conflicting behavior

Keep Google-contract/strict behavior and legacy emulator compatibility distinct.
The working name `fake-gcs-server compatibility profile` is a design label, not
an accepted CLI option. Freeze its activation and configuration with its slice.
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
