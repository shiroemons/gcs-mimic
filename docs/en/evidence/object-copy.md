# Object copy verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on 2026-10-23. The [declared contract](../slices/object-copy.md) owns JSON-OBJECTS-003 and BEHAVIOR-OBJECT-023. The behavior row is IMPLEMENTED/EXACT after the [final integrated gate](derived-objects.md); the broad method remains PARTIAL/EXACT because accepted projection=full omits owner/ACL resources and their additional permissions.

## Executed checks

Security-hardened source `3000cf1f6de5fe2e603b5e63a769024bcded7b52` passes fmt, clippy with warnings denied, all 565 Rust tests, the five targeted pinned Go copy/rewrite/compose tests and vet, and the dependency freshness check. The [appended evidence](derived-objects.md#security-hardening-verification) records the exact scope; containers and guide blocks were not rerun for this change.

The earlier integrated source `285d92f7eec57340c3f4e678761fb118c4d579cf` passed `just pre-push`: fmt, clippy with warnings denied, all 562 Rust tests, the full pinned Go suite with 40 top-level tests and vet, and both linux/amd64 and linux/arm64 container checks. All 31 dependency inputs were current. The [shared evidence set](derived-objects.md#final-source-checks) retains those logs and built local image IDs.

The unchanged copy guide block passes against the native release binary and the final Docker smoke image. An additional composite→copy→rewrite chain preserves exact metadata and bytes across native restart and Docker stop/start and down/up. These affected blocks and their prerequisites were executed; unrelated guide sections were not all rerun.

## Proven behavior and boundaries

Nine copy wire cases cover all source/destination conditions, generation selection, encoded names, same-object/cross-bucket copies, metadata inheritance/replacement, strict separate grants, overwrite delete permission, holds/versioning, empty and multi-buffer bytes, CRC32C/MD5 rejection, fields and composite class/location metadata. Nine deterministic workflow cases cover source/destination races, source read grant revocation before publication, class changes at commit, slow streaming, request ownership, injected advance failure, early advisory rejection, publication admission timeout and shutdown/reopen recovery. The revocation checkpoint proves 403, no destination, no staging files and zero transfer rows while preserving selection-time source conditions. Shared capacity tests additionally prove copy and compose succeed with 128 abandoned rewrites before and after restart.

Copy streams in 16 MiB advances with a 64 KiB buffer. The admission-timeout mutation now fails within a bounded test rather than hanging; the [integration report](derived-objects.md#acceptance-evidence) records the observed failure and restored passing result. Cancellation guards are proven by the targeted handler-drop/advance-failure tests, independently of ordinary HTTP failure cleanup and ephemeral recovery.

Generated Go JSON `Objects.Copy` verifies this operation directly. High-level `ObjectHandle.CopierFrom` uses rewrite; its real one/multi-call workflows now pass in the same integrated SDK suite. Official source pins and exact local metadata/body interpretations are in the contract; provider body/default/error/placement questions remain outside frozen acceptance under SPEC-DERIVED-METADATA and SPEC-COPY-PLACEMENT.

Full owner/ACL projection remains missing. Advanced ACL/KMS/billing/context/retention/temporary-hold inputs return documented errors. Phase-local admission, stalled-catalog HTTP injection, active-write shutdown, real-cloud parity, host power loss and unmeasured resource/performance limits are stated in the [shared boundaries](derived-objects.md#boundaries-and-unperformed-checks).
