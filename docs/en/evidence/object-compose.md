# Object compose verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on 2026-10-23 against the [compose contract](../slices/object-compose.md). JSON-OBJECTS-002 and BEHAVIOR-OBJECT-025, BEHAVIOR-OBJECT-026 and BEHAVIOR-OBJECT-027 are IMPLEMENTED/EXACT in the declared scope after the [final integrated gate](derived-objects.md). The feature ledger is the status authority.

## Executed checks

Security-hardened source `3000cf1f6de5fe2e603b5e63a769024bcded7b52` passes fmt, clippy with warnings denied, all 565 Rust tests, the five targeted pinned Go copy/rewrite/compose tests and vet, and the dependency freshness check. The [appended evidence](derived-objects.md#security-hardening-verification) proves compose capacity remains available with 128 abandoned rewrites before and after restart; containers and guide blocks were not rerun for this change.

The earlier integrated source `285d92f7eec57340c3f4e678761fb118c4d579cf` passed `just pre-push`: fmt, clippy with warnings denied, all 562 Rust tests, all 40 top-level pinned Go tests and vet, and both container architectures. That Rust run includes six compose body-validation cases, 18 wire cases, storage service regressions and three CRC32C combination properties. The full Go suite includes permissive and strict `ComposerFrom` coverage. All 31 dependency inputs were current.

The unchanged compose guide block passes against the native release binary and final Docker smoke image, including source deletion and checksum/byte assertions. An integrated composite→copy→rewrite chain retains exact resources and bytes after native restart and Docker stop/start and down/up. The [single shared evidence set](derived-objects.md#final-source-checks) contains the raw checks, SDK, container, guide and dependency logs, with local prefixes replaced.

## Proven behavior and boundaries

Ordered 1..32 selected same-bucket/class sources support empty, repeated and nested composition. The current quota supersedes the obsolete 1024 limit; component counts saturate at int32 maximum. Combined CRC32C, absent MD5, JSON/XML composite ETags and component headers, metadata/fields/list behavior, holds/versioning and restart preservation are verified.

Strict grants and final publication rechecks, destination races, aggregate size limits, original-error preservation, incarnation changes, late-delete rollback and tagged physical blob reclamation are covered. `deleteSourceObjects` permanently removes selected generations atomically under the declared local safety rules: delete permission, held-source rejection, deduplication and destination-name exclusion.

Pinned Discovery revision 20261013 declares no compose projection parameter, including global parameters. The accidental setting was removed: projection=full/noAcl and other undeclared queries return 400 before mutation; fields selection still succeeds. This is not a missing compose acceptance surface.

Every documented request field is implemented or explicitly rejected. Advanced ACL/encryption/KMS/billing/context/retention/temporary-hold and explicit storageClass inputs return 501. SPEC-COMPOSE-EDGES keeps provider MIME/default/checksum/error/deletion-safety and opaque ETag questions outside the frozen local scope. XML/gRPC compose remain separate features.

The [shared boundaries](derived-objects.md#boundaries-and-unperformed-checks) record resource/admission, active-write shutdown, real-cloud, host-power-loss and benchmark limitations. The fixed 15-minute ephemeral transfer lifetime is not extended by the HTTP request timeout.
