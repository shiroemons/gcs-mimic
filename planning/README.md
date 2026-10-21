# gcs-mimic planning kit

The supplied kit has been unpacked into this repository. Its `project/` contents are at repository root. This directory preserves supplementary material and reusable prompts; there is no nested duplicate project.

The source archive contained 36 files, all verified present after mapping to their organized locations. Its SHA-256 was `450741ecb8376f1389cbeb0dae60539f574731857375746de6b05de495f7fabc`. Canonical documentation, generated views and the ledger were updated deliberately; source inventories and reusable prompts were preserved. The original ZIP is removed after setup verification as requested.

**The storage server, released Docker image and performance results are not available yet.** Current tooling instructions are in the [project README](../README.md); current feature status belongs only to the [ledger](../compatibility/features.json). The selected project name, future CLI binary and repository slug are **gcs-mimic**.

## Language contract

| Material | Language |
| --- | --- |
| Code, identifiers, comments, logs and CLI help | English |
| Commits, issues and pull requests | English |
| Canonical project documentation at repository root | English |
| Implementation prompts and owner-facing work reports | Japanese |
| Optional companion planning material under `notes-ja/` | Japanese |

## Use this kit

1. Use the selected name **gcs-mimic**. The [naming decision](naming.md) preserves the reason and earlier alternatives. Registry availability is separate from this naming decision.
2. The original `project/` has already been installed at repository root. Do not copy it again. Original prompts mentioning that step remain reusable planning instructions.
3. Read the [design](../docs/en/design.md), [roadmap](../docs/en/roadmap.md), [dependency policy](../docs/en/dependency-policy.md), and [onboarding specification](../docs/en/getting-started-spec.md).
4. Give the coding agent [the Japanese master prompt](prompts/00-master.ja.md) and [the bootstrap prompt](prompts/01-bootstrap.ja.md). The first implementation task is P0 only.
5. For each later slice, use [the feature prompt](prompts/02-implement-feature.ja.md) with explicit IDs from the [feature matrix](../docs/en/feature-matrix.md).
6. Update the machine-readable ledger and regenerate the English matrix after evidence supports an implementation-status change. Keep operation, behavior, and project-requirement coverage separate.

All onboarding commands and project CLI names in the specification are proposed behavior for the future implementation. Start with a source Docker build. Add published-image and native-package instructions only after those artifacts actually exist.

## Contents

| Path | Purpose |
| --- | --- |
| `../AGENTS.md` | English implementation rules for coding agents |
| `../compatibility/features.json` | Canonical implementation status, fidelity, sources and evidence fields |
| `../compatibility/gaps.json` | Unresolved specification conflicts and verification tasks |
| `../compatibility/upstream/` | Imported method/schema/protobuf research; reverify before API work |
| `../docs/en/feature-matrix.md` | Complete generated progress matrix |
| `../docs/en/api-inventory.md` | API surface and operation inventory |
| `../docs/en/getting-started-spec.md` | Proposed runtime acceptance journey |
| `../docs/en/dependency-policy.md` | Latest stable dependency policy and current tooling checks |
| `prompts/00-master.ja.md` | Shared Japanese implementation instruction |
| `prompts/01-bootstrap.ja.md` | Foundation-only first implementation task |
| `prompts/02-implement-feature.ja.md` | One incremental feature slice |
| `prompts/03-conformance-audit.ja.md` | Source and implementation audit |
| `prompts/04-onboarding-verification.ja.md` | Full developer journey verification |
| `prompts/05-resource-benchmark.ja.md` | Bounded-memory/CPU evaluation |
| `prompts/06-update-dependencies.ja.md` | Latest stable dependency refresh |
| `notes-ja/` | Japanese planning companions; not a second implementation-status source |

## Scope of the inventory

The baseline was researched on **2026-10-21**. Core JSON Discovery revision **20261013** contains **87 methods and 38 schema definitions**. The inventory also covers separately documented APIs, XML variants, native gRPC, Control, Insights, Batch Operations, Intelligence, enrichment, and MCP. The exact counts and counting rules are in the API inventory.

This is an imported contract baseline, not freshly verified API evidence. It does not claim exhaustive reproduction. Reverify affected official sources and decompose selected rows into field, permission, error and interaction acceptance cases before API work. See the [compatibility policy](../docs/en/compatibility-policy.md).

## Latest stable policy

Use the latest stable Rust and latest non-yanked stable releases of chosen dependencies at implementation time. Recheck official releases at the start of a work slice and before release, with daily automated drift detection after P0. Record the verified toolchain and dependency resolution for reproducibility. Do not freeze the project forever at the versions recorded in this dated plan.

The official Rust release blog reported **Rust 1.99.1**, released **2026-10-15**, at this plan's cutoff. This is a dated observation, not a permanent version instruction. Source: [Rust release announcement](https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/).
