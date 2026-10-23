# Derived object integration verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on 2026-10-23. This is the final shared evidence set for the declared [copy](../slices/object-copy.md), [rewrite](../slices/object-rewrite.md) and [compose](../slices/object-compose.md) contracts. `compatibility/features.json` remains the status authority. The integrated source is `285d92f7eec57340c3f4e678761fb118c4d579cf`, after compose merge `6dff22689bf6b8bfa59d55ed04fc8ef2a1a54096` and rewrite merge `38160733a8534847ee0125810552513063383905`. No dependency was added.

## Final-source checks

| Check | Result |
| --- | --- |
| `just pre-push` | PASS; runs the shared Rust checks, full pinned Go suite and both container architectures once on this source |
| Shared Rust checks | PASS: fmt, clippy with warnings denied, 562 tests, compatibility matrix/schema checks and shell/release checks |
| Full pinned Go SDK suite | PASS: 40 top-level tests and go vet, including generated JSON Copy, high-level CopierFrom one/multi-call rewrite, and ComposerFrom in permissive/strict profiles |
| Container checks | PASS for linux/amd64 and linux/arm64: builds, isolated health, host/DNS access, CRUD, persistence/reset, read-only seeding, configured resource limits and SIGINT/SIGTERM |
| Native affected guide blocks | PASS: copy, one/multi-call rewrite, compose and source deletion; an additional compose→copy→rewrite chain retains exact resources and bytes after restart |
| Docker affected guide blocks | PASS using the final linux/amd64 smoke image without another build; exact resources and bytes persist through stop/start and down/up |
| `cargo xtask dependencies check` | PASS: all 31 compiler, crate, SDK, CI and container inputs CURRENT |

Rust is pinned to 1.99.1. The Go harness uses Go 1.27.3, Storage v1.70.0 and generated API v0.308.0, with explicit guarded loopback endpoints and cloud/metadata access blocked. Container input digests and official contract revision/digests are recorded in the lockfiles, dependency report and individual slice contracts. No published image or real Google bucket was used.

Raw evidence is consolidated into [shared checks](derived-objects-checks.txt), [full Go SDK](derived-objects-sdk-go.txt), [both container architectures](derived-objects-containers.txt), [native/Docker guide commands](derived-objects-guide.txt) and [dependency freshness](derived-objects-dependencies.txt). Only local path prefixes are replaced. The workflow identifiers in the ledger are `scripts/smoke-containers.sh:linux/amd64`, `scripts/smoke-containers.sh:linux/arm64`, `getting-started:copy-rewrite-compose:native` and `getting-started:copy-rewrite-compose:docker`.

Built image IDs are linux/amd64 `sha256:06863a393ce91cbdd07f9ddb48885fb01d4a405d03279b44a635c35277cc07a6` and linux/arm64 `sha256:d8a96aee8631406185470606cda26ea6363cd3275622ec3ef33e852a55e31368`. These are local image IDs, not published registry digests.

BEHAVIOR-OBJECT-023 through BEHAVIOR-OBJECT-027 and JSON-OBJECTS-002 satisfy every declared acceptance case and are IMPLEMENTED/EXACT. JSON-OBJECTS-003 and JSON-OBJECTS-012 remain PARTIAL/EXACT for the explicit full-projection omission. Roadmap orders 9–11 close in this declared scope; the next slice is order 12, local HTTPS/TLS.

## Acceptance evidence

- Copy has nine wire cases and eight deterministic workflow cases. These cover source/destination conditions and races, strict get/create/delete authority, metadata inheritance/replacement, holds/versioning, empty and multi-buffer bodies, checksum failures, stable generations, source replacement, atomic composite metadata, cancellation, advance failure, advisory rejection before staging, bounded publication admission and restart recovery.
- Rewrite covers request-bound random tokens, one/multi-call byte budgets, immediate retry without duplicate publication, strict per-call grants, expiration/eviction, corruption and unknown-token rejection, selected source state, persisted hash/offset recovery, final atomic destination publication and class/component metadata chosen in that transaction.
- Compose covers ordered 1..32 sources, repeated/empty/nested sources, selected generations, saturating component counts, combined CRC32C and absent MD5, JSON/XML ETags, class/hold/versioning interactions, strict grants, destination races and all-or-nothing selected-source hard deletion. Pinned Discovery revision 20261013 declares no compose projection parameter; both projection values return 400 before mutation, while fields selection succeeds.
- The shared foundation has persistent bounded streaming/hash checkpoints, immutable source links, foreign-key binding teardown, request ownership, fixed expiry, rollback, corrupt-prefix quarantine, rename recovery, tagged post-commit reclamation and cleanup retry tests. A published catalog reference to a missing blob remains a readiness failure.

Copy uses 16 MiB advances through the same 64 KiB streaming buffer, reducing checkpoint/fsync frequency. No throughput, CPU or memory improvement is claimed. Begin/advance catalog activity suppresses the idle connection watchdog while the request deadline owns those waits; publication admission has a separate budget and an admitted publication returns its durable outcome.

The publication-admission regression has an outer five-second test bound and releases its blocker before asserting. Temporarily removing the production admission deadline failed in 5.90 seconds with `publication admission did not time out: Elapsed(())`; restoring the final source passes. This is an intentional mutation check, not an unresolved test failure. The advisory-before-staging regression verifies that a failed create-if-absent request never reaches the transfer checkpoint.

## Boundaries and unperformed checks

Full owner/ACL projection and its additional permissions remain missing for the accepted copy/rewrite projection=full setting, so their broad method rows remain PARTIAL. Unsupported ACL, encryption/KMS, requester billing, contexts, retention, temporary holds and advanced fields return documented errors. Provider body/default/error/token/placement questions remain outside the frozen local scope in the named gap ledger.

Admission remains phase-local; competing load can reject a later advance or publication. The shutdown regression interrupts between advances; it does not independently inject catalog close during an active file write. The connection watchdog under a deliberately stalled catalog is not separately exercised over HTTP. Cancellation guard behavior is proven by the targeted handler-drop/advance-failure tests; ordinary HTTP error cleanup and ephemeral restart recovery alone do not establish that guard behavior.

No real-cloud differential request, provider replication/large-object placement simulation, process RSS/cgroup benchmark, CPU/throughput benchmark, host power-loss test or exhaustive per-instruction fsync crash campaign was performed. The final guide scope is the affected copy/rewrite/compose blocks and their prerequisites, plus an integrated restart chain; it does not claim reexecution of every unrelated guide section.
