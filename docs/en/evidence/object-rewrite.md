# Object rewrite verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on 2026-10-23 against the [rewrite contract](../slices/object-rewrite.md). BEHAVIOR-OBJECT-024 is IMPLEMENTED/EXACT after the [final integrated gate](derived-objects.md). JSON-OBJECTS-012 remains PARTIAL/EXACT for accepted projection=full without owner/ACL resources and their additional permission behavior.

## Executed checks

Final source `285d92f7eec57340c3f4e678761fb118c4d579cf` includes reviewed rewrite `fb71316f790c85a29d9e936965511fef45c3fc17` and passes `just pre-push`: fmt, clippy with warnings denied, all 562 Rust tests, all 40 top-level Go SDK tests plus vet, and linux/amd64 and linux/arm64 container checks. All 31 dependency inputs are current.

The full SDK suite includes the official high-level `ObjectHandle.CopierFrom` in one-call, cross-location multi-call, exact-boundary and empty-object cases. The unchanged one/multi-call rewrite guide blocks pass against the native release and final Docker smoke image. An integrated composite→copy→rewrite chain preserves exact resources and bytes after native restart and Docker stop/start and down/up. Raw outputs are consolidated in the [shared evidence set](derived-objects.md#final-source-checks).

## Proven behavior and boundaries

The frozen local session scope covers random request-bound tokens, bounded progress, fixed one-week expiry, immediate-previous-token replay, one atomic destination/completion response, strict per-call grants, selected source state and preconditions, process interruption/restart, corruption rejection and bounded active/completed retention. Same-location/class rewrites finish in one call; the positive one-MiB multiple applies across modeled location/class changes.

The final transaction derives composite component count from the actual destination class/location. Source incarnation/generation/metageneration, final destination conditions and create/delete permissions are rechecked; durable response replay cannot create another generation. Cleanup failures preserve the original result, and completed responses remain authoritative if a network receiver disappears.

Unsupported KMS/CSEK, ACL/owner, requester billing, contexts/retention and advanced metadata fail explicitly. SPEC-REWRITE-EDGES and SPEC-DERIVED-METADATA retain provider token/error/replay/source-change and body/default questions outside the declared local acceptance scope. The local active/completed limits, creation-deadline eviction and phase-local admission do not claim provider infrastructure behavior.

No real-cloud, comparative performance/resource or host-power-loss claim is made. The [shared boundaries](derived-objects.md#boundaries-and-unperformed-checks) distinguish tested process recovery from exhaustive durability and active-write shutdown injection.
