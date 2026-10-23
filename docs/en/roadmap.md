# Incremental implementation roadmap

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

## Delivery model

Complete a useful, independently verifiable capability before expanding its
scope. The execution plan is `delivery_plan` in
[`compatibility/features.json`](../../compatibility/features.json). It names
existing evidence, ordered deliverables, prerequisites, completion conditions
and exclusions. This document explains that plan; it owns no implementation
status.

The existing P0-P7 assignments remain coverage groups for inventory and
historical evidence. They are not whole-group execution gates. In particular,
P0 clock injection does not wait for lifecycle maintenance, and P1 single-request
upload does not wait for resumable transfer, signing or gRPC. Those extensions
have their own deliverables. Existing later work is retained even when an
earlier verification task remains open.

No feature status, fidelity, acceptance result, test ID or existing evidence
was changed to adopt this plan. A planning entry is neither an implementation
claim nor permission to start implementation.

The product's minimum functional target is the complete pinned reference
capability set, with gcs-mimic additions above it. For each affected feature,
inventory source-supported inputs, response information, configuration, state
effects and existing-client workflows before extending the native scope.
Source-equivalent functionality needs executable evidence; a narrower shared
happy path or extra strict validation does not establish a superset. Conflicting
legacy behavior belongs in an explicit compatibility profile, with native and
strict Google-contract behavior tested separately.

## Foundation gate

Before starting another capability, check the existing evidence for the
declared development profile and run only missing or affected checks:

- Build and start the pinned binary and supported source-built container.
- Validate configuration precedence, health/readiness and graceful shutdown.
- Recover persisted catalog/blob state before readiness and retain exclusive
  data-directory ownership.
- Use the injected clock, bounded transfer/catalog admission and indexed lookup.
- Pass ledger/generated-view and relevant Rust checks.

This gate concerns the runnable baseline. Local HTTPS/TLS and CORS are separate
required pre-signing deliverables below. Fixture extensions, maintenance state
machines, all-platform distribution and public publication also have their own
verification. These deliverables do not retroactively extend the
foundation's completion scope. Selected crash recovery is not power-loss proof.

## Existing capabilities and evidence

These entries show the declared verified scopes. Whole-feature completion is
recorded only in the ledger. Existing source versions, command outputs and limits
remain in the named evidence files.

| Capability | Verified scope | Remaining boundary | Evidence |
| --- | --- | --- | --- |
| Runtime foundation | HTTP, configuration, probes, non-root container and injected clock | Current-source platform/CI and release claims need their own evidence | [Runtime/container](evidence/distroless.md) |
| JSON bucket CRUD | Persistent create/get/list/delete, conditions and scoped paging | IAM/ACL, ownership and advanced settings | [Bucket verification](evidence/bucket-crud.md) |
| Bucket metadata PATCH | Label merge/removal, supported default class, versioning/default holds, atomic conditions, strict permission, restart and HTTP/Go/native/Docker checks | Whole method and affected broad behavior rows remain PARTIAL for advanced settings and provider edge parity | [Final PATCH verification](evidence/bucket-metadata-patch.md) |
| Bucket metadata PUT | Replacement/local omission defaults, settings effects, atomic conditions, strict permission, resource preservation and final Rust/Go/reference/container/native/Docker checks | Whole method remains PARTIAL; advanced fields, ACL/billing/projection and provider parity pending; API dependency update detected | [Final PUT verification](evidence/bucket-metadata-put.md) |
| Object media | Live-generation upload/read/delete, hashes and atomic conditions | Header/error parity, token replay and advanced semantics | [Media verification](evidence/object-media.md) |
| List and metadata | Prefix/delimiter/keyset paging; PATCH merge and PUT replacement | History, filters, full projection and legacy-input parity | [Integrated verification](evidence/object-list-metadata.md) |
| JSON multipart | Bounded metadata/media/checksum containers and Go single-request Writer | MIME edge parity, replay and advanced fields | [Multipart verification](evidence/multipart.md) |
| Local strict authentication | Local tokens, project-level predefined roles and pure URL builder | Signatures and full strict behavior | [Authentication verification](evidence/strict-auth.md) |
| Bucket IAM policies | Persistent unconditional predefined-role bindings, project-plus-bucket enforcement and atomic etags; native Go and strict Compose journey | Whole policy methods and inherited hierarchy remain PARTIAL; conditions, custom/basic roles, ACLs and wire parity remain | [Final IAM verification](evidence/bucket-iam.md) |
| Public object reads | Verified path-style XML GET/HEAD, live-generation selection, single ranges, match conditions, public/registered-only grants, revocation/restart and pinned Go/host/peer journeys | Virtual-host routing, XML writes/listing, ACL/UBLA/PAP state machines, signatures and complete XML/authentication wire parity | [Final public-read verification](evidence/public-object-reads.md) |
| JSON resumable upload | Sessions, bounded chunks/status/retry, atomic finalization, cancellation, fixed seven-day expiry, selected process-kill/restart recovery, client-reachable configured Location and normal pinned Go Writer; measured native large/slow/concurrent buffering | XML/signed initiation, token response replay, provider edge parity, power-loss durability and Linux/cgroup memory guarantees | [Final resumable verification](evidence/resumable-upload.md) |
| Single JSON range | Closed/open/suffix stored-byte reads, 206/416, pinned generation and bounded seeking | If-Range, transcoding and exact fallback/header parity | [Range verification](evidence/object-range.md) |
| Initial seed import | Import before readiness without replay; Linux invalid-byte rejection and read-only Compose persistence/reset | Manifest metadata, legacy defaults and source mutation protection remain outside the native contract | [Seed verification](evidence/seed-platform.md) |

`BEHAVIOR-OBJECT-016` remains IMPLEMENTED for its declared single-range subset;
`JSON-OBJECTS-005` remains PARTIAL. `UX-002` and `UX-009` are now IMPLEMENTED:
the missing Linux/Compose cases pass with recorded evidence. ARM64 uses native
execution and AMD64 uses emulation. Broader fixture/volume requirements retain
their explicit remaining scope. Range and seeding are existing work, not the
next features to reimplement. The integrated [native guide](evidence/seed-range-guide.txt)
and [Go SDK log](evidence/seed-range-sdk.txt) retain their actual execution scope.

## Ordered delivery queue

The order below is the default selection order. Explicit prerequisites in the
ledger determine readiness; independent verification or Ruby integration may
run earlier. Finish one bounded slice per implementation turn. Within a
deliverable, validate each step before adding the next; an initiation route or
success stub does not complete a workflow.

| Order | Deliverable | Completion boundary | Visible user outcome |
| ---: | --- | --- | --- |
| 1 | Seed platform verification | Linux filename and seeded Compose persistence/reset cases passed on 2026-10-22 | Missing platform evidence recorded; UX-002 and UX-009 IMPLEMENTED |
| 2 | Bucket IAM policies | Declared unconditional policy scope passed final Rust, Go, both Linux container architectures and full guide verification on 2026-10-22 | Delivered: policy operations affect access; DESIGN-014 and DESIGN-027 IMPLEMENTED, policy methods and hierarchy PARTIAL |
| 3 | Public object reads | Declared path-style XML GET/HEAD, allUsers versus registered-only grants and final Rust/SDK/container/guide checks passed on 2026-10-22 | Delivered: DESIGN-012, DESIGN-013, DESIGN-047 and UX-005 IMPLEMENTED; DESIGN-004 and broad XML methods PARTIAL; [final verification](evidence/public-object-reads.md) |
| 4 | Resumable upload | Declared initiation/chunk/status/retry/finalization/cancel/expiry/Location cases, normal pinned Go Writer, native buffering measurements and selected process-kill recovery passed on 2026-10-22 | Delivered: BEHAVIOR-OBJECT-010 through BEHAVIOR-OBJECT-014 and DESIGN-057 IMPLEMENTED; JSON-OBJECTS-007 and DESIGN-082 remain PARTIAL; [final verification](evidence/resumable-upload.md) |
| 5 | Bucket create, get and delete | Declared native/compatibility creation, basic GET, empty deletion, versioning/default holds and HTTP/SDK/container/guide checks passed on 2026-10-22; both pinned reference backends compared | Delivered: named baseline with explicit intentional/strict/unsupported differences; broad bucket and versioning/hold rows remain PARTIAL; [final verification](evidence/bucket-core-parity.md) |
| 6 | Bucket metadata PATCH | Declared label merge/removal, default class, versioning/default holds, conditions, strict permission and persistence passed final Rust/Go/container/guide checks on 2026-10-23; both pinned reference backends compared | Delivered: change basic settings without recreating the bucket; JSON-BUCKETS-008 and affected broad behavior rows remain PARTIAL; [final verification](evidence/bucket-metadata-patch.md) |
| 7 | Bucket metadata PUT | Declared replacement/local defaults, conditions, strict permission and resource preservation passed final Rust/Go/reference/both-architecture container/native/Docker checks on 2026-10-23, including Docker restart equality | Delivered: replace the modeled settings; JSON-BUCKETS-013 remains PARTIAL; [final verification](evidence/bucket-metadata-put.md) includes the verified API client update |
| 8 | Basic JSON contract | Frozen live-resource profile, documented single conditions/checksums/error envelopes, fields/read ETags and remaining-gap mapping passed final Rust/Go/reference/both-architecture containers and native/Docker guide checks on 2026-10-23 | Delivered: BEHAVIOR-COMMON-007/011 and BEHAVIOR-OBJECT-005 IMPLEMENTED in their declared scope; broad method rows remain PARTIAL; [final verification](evidence/basic-json-contract.md) |
| 9 | Object copy | Generation-pinned streaming, metadata, strict permissions, atomic conditions, versioning/holds, composites and restart pass Rust/targeted Go checks; [evidence](evidence/object-copy.md); final integration gates pending | Exact frozen copy subset is delivered; behavior awaits integration, method additionally lacks full ACL/owner projection |
| 10 | Object rewrite | Bounded progress, request-bound continuation tokens, one atomic destination and restart/retry evidence | Complete one-call and multi-call rewrites |
| 11 | Object compose | Ordered bounded sources, component/checksum rules and explicit source-deletion semantics | Concatenate objects into one destination |
| 12 | Local HTTPS/TLS | HTTP-only/HTTPS-only modes, supplied certificate/key, bounded handshake, verified client trust, advertised endpoints and native/container/browser checks | Use local HTTPS without disabling certificate or hostname verification |
| 13 | JSON CORS defaults | Official JSON simple/preflight/default-header rules independent of bucket settings; real-browser JSON CRUD and permission denial | Use basic JSON APIs from a different browser origin before signing |
| 14 | Bucket/XML CORS | Persistent JSON bucket GET/PATCH/PUT CORS settings; XML GET/HEAD and preflight allow/deny/header rules; real browser and restart | Configure bucket CORS and use existing XML reads from a browser |
| 15 | Basic API, CORS and HTTPS verification | All preceding scopes; HTTP coverage, pinned Go/Ruby, actual-browser CORS over trusted local HTTPS, failure/concurrency/restart and final native/container journeys | Evidence-backed basic bucket/object APIs, CORS and HTTPS before signed URLs |
| 16 | V4 RSA signed download | Basic verification gate, final-host canonicalization, signer permission, expiry and tamper rejection | Private objects can be downloaded with validated signed GET |
| 17 | V4 RSA signed upload | Bounded XML PUT, signed headers/payload rules, atomic conditions and failure preservation | Validated signed PUT works |
| 18 | Signed browser transfers | Reuse implemented CORS/TLS; real-browser signed upload/download, allowed/denied origins, preflight and exposed headers | Complete signed browser workflows over HTTPS |
| 19 | Ruby/Rails workflows | Extend the earlier basic Ruby SDK evidence to actual Active Storage public/private delivery plus direct uploads | Tested Rails/browser integration |
| 20 | Release verification | Basic API/CORS/HTTPS gate, required CI/native targets, freshness, notices, checksums and explicitly authorized publication | Reproducible downloadable artifacts |

Order 8 is closed by the [final basic JSON evidence](evidence/basic-json-contract.md).
The [frozen live-resource contract](slices/basic-json-contract.md) maps completion
conditions 1, 3 and 4 to executed cases and explicit broad-method/gap destinations;
condition 2 retains its [fields/read-validator evidence](evidence/basic-json-fields-etag.md).
Documented single conditions, full-byte checksum validation and error envelopes
are IMPLEMENTED for their declared EXACT scopes. Mutation NotMatch status,
mixed/missing-resource precedence, multipart header and multi-request resumable
MD5 questions remain open; every original case, deviation and historical
verification is preserved. Broad API methods remain PARTIAL.
Order 9 **Object copy** has its [declared local implementation](slices/object-copy.md)
and [Rust/targeted Go evidence](evidence/object-copy.md). Final-source container,
full SDK and native/Docker guide gates remain pending; the two broad ledger rows
stay PARTIAL until that gate; the method additionally lacks full ACL/owner projection.
Provider metadata/error/placement questions remain outside declared local acceptance.
The next implementation slice is order 10 **Object rewrite**, using the shared
persistent derived-transfer foundation; compose remains order 11.
Order 7 is closed by the [final PUT evidence](evidence/bucket-metadata-put.md),
covering the [frozen replacement contract](slices/bucket-metadata-put.md),
Rust/Go checks, both reference backends and local profiles, both container
architectures and affected native/Docker guide blocks with restart equality.
Separate maintenance adopted API client v0.308.0 with Storage v1.70.0 unchanged;
the full SDK suite passes and all 31 dependency inputs report CURRENT.
The complete buckets.update row stays PARTIAL for advanced fields,
ACL/billing/full projection and provider defaults/error parity.
Order 6 is closed by [final PATCH evidence](evidence/bucket-metadata-patch.md),
including effective versioning/default-hold changes, label merge/removal,
clock/revision behavior, native/Docker guide checks and explicit pinned-reference
differences. Advanced fields and Google-service questions remain pending.
Order 5 is closed by [final bucket evidence](evidence/bucket-core-parity.md),
including API-created settings that drive actual object effects, source-only
names across object/session/XML routes and both pinned reference backends.
Broader method/behavior rows retain their pending Google-contract cases; no
full replacement or whole-method parity is claimed.
Signed download depends explicitly on the
basic API, CORS and local HTTPS verification gate.
These are execution priorities, with no calendar delivery dates assigned.
The resumable deliverable is closed by [final evidence](evidence/resumable-upload.md),
including five native process-kill boundaries and their 20 repeated runs;
broader provider edge parity, other platforms and power loss remain unverified.
The scoped bucket IAM deliverable is closed by the
[final integration evidence](evidence/bucket-iam.md),
while the whole policy-method and hierarchy rows retain their explicit missing
cases. The preceding seed gate is closed by the
[Linux and Compose evidence](evidence/seed-platform.md). The committed
[public-read subset](slices/public-object-reads.md) serves path-style XML GET/HEAD
with persistent bucket grants; its final integrated verification is recorded in
[the public-read evidence](evidence/public-object-reads.md). This does not complete
the broader XML, authentication or access-control feature rows.

### Basic JSON API milestone before signed URLs

The milestone's fixed operation set is:

| Surface | Included methods | Supported basic scope |
| --- | --- | --- |
| Buckets | `insert`, `get`, `list`, `delete`, `patch`, `update` | Live buckets; modeled identity/metadata, project/prefix paging, labels and supported default storage class; atomic metageneration checks and strict local grants |
| Objects | `insert`, `get`, `list`, `delete`, `patch`, `update`, `copy`, `rewrite`, `compose` | Live objects; media/multipart/JSON-resumable upload, content/custom metadata, whole/single-range reads, prefix/delimiter/offset paging, checksums, source/destination conditions and bounded transfers |

The bucket core gate additionally covers the pinned source's supported
configuration and its interactions with these operations. Those required source
effects are not excluded merely because their historical group is P4. Keep
broader Google semantics and capabilities outside the selected source baseline
as separate pending work.

Local HTTPS/TLS, JSON CORS defaults and scoped bucket/XML-read CORS are also
required before signing. They add transport/browser acceptance scopes, not
additional API-method counts. Implement each as its own slice and validate the
combined behavior at the basic verification gate.

For HTTPS, supply a development certificate chain and private key, preserve
HTTP-only operation and add HTTPS-only operation. Validate configuration before
readiness and bound handshakes within connection admission. Test trusted local
CA/SAN fixtures with native, container, SDK and browser clients; untrusted CA,
wrong hostname, expiry, invalid key/configuration and stalled handshake cases
must fail safely. Keep listener and advertised endpoints distinct and verify
HTTPS resumable Locations. Do not disable client certificate verification or
claim HTTPS merely because generated URLs use that scheme. Public certificate
issuance, system trust-store changes, mTLS, hot reload and dual listeners remain
separate work.

JSON CORS uses the documented default response rules regardless of bucket CORS
configuration. XML CORS follows stored bucket origin/method/header/max-age rules.
Persist those rules through JSON bucket GET/PATCH/PUT with validation,
metageneration conditions and strict permission checks. Test actual cross-origin
JSON upload/read/list/update/delete and existing XML GET/HEAD, preflight and
exposed headers, denied object access after allowed preflight, and restart.
The [official CORS contract](https://docs.cloud.google.com/storage/docs/cross-origin)
defines the protocol distinction. XML bucket `?cors` configuration routes and
general XML writes remain outside this scoped delivery. A preflight success
does not prove an XML upload works; real signed PUT transfers are checked after
the XML-write/signing slice exists, in the later signed-browser gate.

The basic-contract deliverable now includes response `fields` selection over
supported resources and JSON read ETag validators, including policy GET and
combined query conditions. The two documented behavior scopes are IMPLEMENTED
with unchanged EXACT targets. Undocumented edges and provider wire questions
remain on named gaps and PARTIAL method rows. Copy, rewrite and
compose each have separate completion conditions, including failure preservation
and source/destination races. Rewrite must exercise continuation; an immediate
copy stub is insufficient. PATCH and PUT require distinct merge/replacement
tests, including omitted/null/default and immutable-field handling reconciled
against current sources before implementation.

If-Range is excluded from this milestone. The Google JSON contract remains
unestablished in `SPEC-RANGE-VALIDATORS` in the
[gap ledger](../../compatibility/gaps.json); retain the current explicit 501
rejection until authoritative sources establish it. Contract research and any
resulting implementation are separate later work and do not block basic JSON
completion, copy/rewrite/compose or signed URLs. The broad `objects.get` row
retains its pending cases and `PARTIAL` status.

Close the milestone only after linked HTTP evidence covers all six bucket and
nine object methods and pinned Go/Ruby workflows exercise the applicable calls.
Verify denied access, invalid input, stale conditions, checksum rejection,
paging, concurrent mutations and restart persistence. Use isolated local
endpoints; signed URLs and browser uploads are not prerequisites for basic SDK
coverage. Include trusted local HTTPS and actual-browser JSON/XML CORS checks.
Run final-source native/container journeys and relevant regressions;
unchanged evidence requires an explicit source/input comparison.

Excluded work beyond the selected source baseline remains visible on the broad
feature rows: additional history/protection semantics, soft delete/restore,
retention/lifecycle, glob/context filters, transcoding,
token-keyed response replay, object IAM/ACL, conditional IAM, full UBLA/PAP,
billing/encryption infrastructure and undocumented provider edge parity.
`objects.move`, bucket relocation/storage layout and the IAM/test-permission
methods retain their separate P2-P7 work. Signed URLs, signed-browser transfer
integration and full Rails direct uploads follow this milestone. Initial CORS
and local HTTPS implementation are prerequisites of this milestone.

This gate does not require whole-method `IMPLEMENTED` status and does not shrink
existing acceptance cases. It requires every case in the declared basic scope
to pass, with remaining broader cases explicitly pending. README comparison
checkmarks apply to the same named workflow for every product; whole-method
completion and fidelity remain solely in the feature ledger. Existing scoped
checkmarks may be corrected from existing evidence, without waiting for this
new milestone or claiming it has passed.

### Small steps inside the larger workflows

| Deliverable | Step order | Completion evidence |
| --- | --- | --- |
| Public reads | Policy get/set and enforcement; then grants and XML GET/HEAD | Wire permission/etag tests, restart, encoded paths and Go default/XML reads |
| Resumable upload | Initiation/status; then chunk/retry/finalization; then expiry/cancellation/recovery | Interrupted and duplicate chunks, atomic visibility, checksum rejection and pinned Go Writer |
| Basic APIs and transport | Bucket core source parity; bucket PATCH; bucket PUT; basic-contract gaps; copy; rewrite; compose; HTTPS; JSON CORS; bucket/XML CORS; integrated gate | Source-compatible bucket cases, explicit six-bucket/nine-object scope, trusted local TLS, browser/SDK/wire cases and final-source evidence before signing |
| CORS and HTTPS | Implement independent TLS, JSON defaults and bucket/XML-read slices; combine them before signing; integrate signed browser transfers later | Trusted-CA and negative certificate tests, real-browser preflight/access tests and persistent CORS configuration |
| Signed access | Signed GET first; XML PUT and signed PUT next | Valid, expired, tampered, wrong-host and insufficient-permission requests |
| Copy/rewrite/compose | Finish copy; then rewrite continuation; then compose | Source/destination races, token retry/restart, failed-overwrite preservation and bounded streaming |
| Ruby/Rails | Verify basic Ruby SDK calls before signing; finish later with the complete Rails/browser journey | Version-pinned SDK and real application/browser execution |

Resumable sessions are bearer capabilities with their own retry/status model.
They do not require signed URLs to exist. Generation conditions, accepted
invocation tokens and resumable offsets are not token-keyed response replay;
SPEC-013 remains open until that behavior is implemented and evidenced.

Before public grants, model the supported access restrictions and reject
recognized unsupported policy semantics rather than ignoring them. Full
ACL/default ACL, UBLA/PAP transitions and conditional IAM remain separate
acceptance work; no public-read slice may bypass a restriction it claims to
support. Signed access must preserve the supported PAP distinction between
anonymous public grants and authorized signed requests.

## Migration track and prerequisites

The product target is a complete fake-gcs-server behavioral superset after
renaming environment keys and explicit data conversion. The acceptance contract
is [the migration contract](fake-gcs-server-migration.md); its milestones live in
`delivery_plan.migration`. They add no implementation status.

Before each capability slice, reconcile only its affected contracts and profile
conflicts. The complete source inventory is a full-replacement verification
prerequisite, not a prerequisite for bucket create/get/delete work. Keep it on
the migration track and add each missing capability as its own bounded slice.
The existing queue is an early useful release path; completing it alone does
not establish full replacement. No source surface disappears from the final
gate because it lacks a current GCS inventory row.

| Order | Migration deliverable | Prerequisite | Completion boundary |
| ---: | --- | --- | --- |
| 1 | Source contract baseline | Runtime foundation | Complete flags/routes/Go/backend inventory and executable legacy/strict expectations |
| 2 | Seed adapter | Baseline and native seed | Preserve source keys, bytes, MIME/default metadata and selected seed lifecycle |
| 3 | Durable filesystem import | Baseline, object media and metadata | Read xattrs/sidecars without modifying source; preserve identities and verify recovery |
| 4 | Go and live-state export | Baseline, durable import, public reads and resumable upload | Tested Go facade; export active/archived state before stopping memory source |
| 5 | Full replacement validation | Complete source inventory, seed adapter, Go/live-state, compose and Ruby/Rails | Every baseline capability and preservation case passes; no undisclosed exclusions |

The diagram groups related prerequisites. It shows no execution results;
the ledger retains the exact dependencies and default selection order.

```mermaid
flowchart TD
  E[Existing verified capability subsets] --> B[Source contract baseline]
  E --> S[Seed platform evidence recorded]
  E --> I[Bucket IAM]
  E --> R[Resumable uploads]
  I --> P[Public reads]
  E --> F[Scoped contract then create get delete]
  F --> M[Bucket PATCH then PUT]
  M --> J[Basic JSON contract]
  R --> J
  J --> Q[Basic API CORS and HTTPS verification]
  P --> U[Signed GET then PUT]
  Q --> U
  I --> C[Copy then rewrite then compose]
  J --> C
  C --> Q
  E --> T[Local HTTPS/TLS]
  J --> O[JSON CORS defaults]
  M --> X[Bucket and XML-read CORS]
  P --> X
  O --> X
  T --> Q
  O --> Q
  X --> Q
  U --> W[Signed browser and Rails workflows]
  P --> W
  R --> W
  B --> A[Source seed adapter]
  E --> A
  B --> D[Durable data import]
  E --> D
  D --> G[Go adapter and live-state export]
  B --> G
  P --> G
  R --> G
  A --> V[Full replacement validation]
  B --> V
  C --> V
  W --> V
  G --> V
```

For settings, replace `FAKE_GCS_` with `GCS_MIMIC_` and retain suffix/value
semantics; legacy keys produce a rename diagnostic, never aliases or fallback.
Seed input and persistent destination remain separate. Existing Compose
`GCS_MIMIC_PORT` publishes a host port; do not silently reinterpret it as the
container listener. Equivalent settings, importers and compatibility profiles
have not been implemented.

## Completion and progress reporting

Each deliverable uses the ledger's common completion gate:

1. Freeze protocol/profile, supported fields, limits and exclusions; reconcile
   current official sources and affected gaps before implementation.
2. Name success, error, boundary, permission and feature-interaction cases.
3. Pass wire and applicable pinned SDK/CLI/container checks; verify relevant
   concurrency, interruption, restart, resource and clock properties.
4. Resolve deviations inside that scope and attach commands, results, test IDs
   and durable evidence. Retain explicit simulation/external boundaries.
5. Update the affected feature records and generated views. Apply the unchanged
   [IMPLEMENTED policy](compatibility-policy.md) to each whole feature row.

The existing acceptance references are navigation aids, not proof that every
new completion condition passes. Planning entries have no independent status.
At a delivery checkpoint, report the named completed scopes and their evidence
against a scope fixed before work started. State any scope change explicitly.
Do not derive a completion percentage merely from this planning metadata.

Report API operations, behavior and project requirements separately. Do not
count overlapping references as independent GCS features or assign PARTIAL a
50% weight. A broad method can remain PARTIAL while a narrow, independently
evidenced behavior is IMPLEMENTED. A later extension does not invalidate a
completed scope unless the existing behavior regresses.

README capability summaries should name the tested operation/profile and its
limits. Comparison marks must use the same named scope across products; they
must not be upgraded merely because the roadmap was reorganized.

## Coverage retained for later delivery

These group assignments remain unchanged in the feature ledger. The delivery
queue above takes precedence over their historical numeric ordering.

| Group | Remaining coverage |
| --- | --- |
| P0 Foundation | Local HTTPS/TLS is required before signing; maintenance extensions, full legacy dual listeners and platform/distribution evidence retain separate scopes |
| P1 Core data path | Basic bucket updates, supported fields and HTTP conditions are scheduled before signing; advanced fields, transcoding and broader resource guarantees remain later |
| P2 Access and URLs | JSON and bucket/XML-read CORS are required before signing; remaining ACL/default ACL, UBLA/PAP, conditional policies, HMAC/V2 and endpoint breadth retain separate scopes |
| P3 Transfer and HTTP breadth | Rewrite continuations, HTTP batch, remaining XML/multipart and extended SDKs |
| P4 Protection and lifecycle | Versioning, soft delete/restore, holds, retention, lifecycle and Autoclass |
| P5 Hierarchy and control | HNS/managed folders, inherited access, storage layout and shared LROs |
| P6 gRPC and Rapid | Storage v2 RPCs and appendable-object state machines |
| P7 Extended capabilities | Caches, analytics/jobs/events, contexts, infrastructure adapters and extended clients |

## First useful release

The first developer release retains the P1 core plus essential P2 public/private
and URL target: documented JSON workflows, the supported range/seed subsets,
strict local identity, effective scoped bucket IAM and public XML reads.
The basic milestone above, including Go/Ruby evidence, CORS and trusted local
HTTPS browser journeys, is a release verification prerequisite. Signed URLs and
Rails/signed-browser workflows remain later capabilities with their own gates.
The scoped JSON resumable workflow has
separate evidence. State supported profiles and resource, recovery and protocol
limits in the release; unverified clients remain explicit limitations.

Do not wait for lifecycle, gRPC, analytics or every row in P0/P1/P2 to complete.
Do require the release's declared feature/profile/platform tests, dependency
freshness, relevant resource measurements and the existing
[release policy](release-policy.md). Native/image workflow configuration is not
execution evidence. Publishing remains a separately authorized action.

## Source and storage maintenance

Retain the dated source lock, refresh Discovery/protobuf/release restrictions
before affected slices and releases, and keep new/retired methods visible.
No real-cloud differential test is implied; unresolved source questions remain
in `compatibility/gaps.json` until explicitly authorized evidence resolves them.

Keep SQLite plus opaque immutable files as the recommended durable default.
The storage comparison in `docs/en/storage-backends.md` evaluates BLOB,
key/value, filesystem and memory alternatives. Foreign-data migration is a
planned capability, separate from replacing the internal engine. Implement no
backend or importer in this documentation change. Reconsider the default after
a measured requirement justifies its recovery and compatibility cost.
