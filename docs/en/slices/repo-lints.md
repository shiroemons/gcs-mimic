# Repository lint contract

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

## Scope

This slice adds compile-time policy only, with no runtime or storage changes.
The application and `xtask` inherit `unsafe_code = "deny"` from workspace lints.
They also inherit `clippy::undocumented_unsafe_blocks = "deny"`, requiring
adjacent `// SAFETY:` comments on each permitted unsafe block.
Existing platform and test FFI uses receive local exceptions with safety reasons;
neither a crate nor a complete storage or test module receives an exception.
An unsafe callback's item-level exception also covers its body and nested items;
explicit unsafe blocks and safety comments remain lint-checked, while review must
check all operations and nested items within that exception for necessity and safety.

No feature IDs are assigned to this repository-quality slice.
`compatibility/features.json` and its generated views remain unchanged.
Shared-state API, startup, publication and benchmark decisions belong to their
respective slices and are not changed here.

## Acceptance cases

| Case | Verification |
| --- | --- |
| The application and `xtask` inherit the workspace unsafe denial, including integration tests. | Inspect both manifests and verbose workspace/all-target Clippy invocations. |
| An unsafe block without a local exception is rejected; a justified local exception compiles. | Compile a small stdin probe using the configured lint level. |
| An allowed unsafe block without a safety comment is rejected. | Inspect verbose workspace/all-target Clippy invocations and run negative and positive probes with the undocumented-unsafe-blocks lint denied. |
| Existing SQLite callback/limit and Unix fork tests retain their behavior. | `just check` runs all existing workspace tests without skips or runtime changes. |
| Windows publication and console-control FFI retain narrow, documented exceptions. | Review every Windows unsafe site and attempt Windows-target Clippy; report unavailable cross-compilation prerequisites. |
| Future parsing modules deny unwrap/expect without changing legacy modules. | Document module-scoped attributes and verify a small Clippy probe rejects both calls. |
| Parser lint opt-ins and unsafe exception scopes are checked during subsequent slice reviews. | Document the required review checks, including callback bodies and nested items; keep the shared SQLite callback unsafe with a documented pointer contract and an explicit inner unsafe block. |
| The future filesystem helper has an explicit exception procedure. | Document how its owning slice moves and reviews exceptions when creating `src/fsutil.rs`. |

## Policy and exclusions

See [the development lint policy](../development.md#rust-lint-policy) for new
workspace members, parsing modules and platform-specific exceptions.
Dependencies, lockfiles and feature implementation statuses are outside this
slice. No performance claim or benchmark is needed for lint attributes alone.

Future proposal: approve rusqlite's test-only `hooks` feature to remove unsafe.

See the [verification evidence](../evidence/repo-lints.md) for results and limits.
