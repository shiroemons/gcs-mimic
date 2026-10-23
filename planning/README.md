# gcs-mimic planning kit

This directory preserves the initial planning kit and reusable implementation
prompts. The project's runtime, documentation and checks are at repository root.

For current usage and priorities, read the [project README](../README.md),
[Japanese user guide](../docs/ja/README.md) and [roadmap](../docs/en/roadmap.md).
The server now exists; public image distribution is still pending. Initial kit
statements about future implementation are historical, not current status.

## Language and status

English is primary for code, CLI, logs and canonical documentation. Japanese user
documentation lives in `docs/ja`; implementation prompts and owner communication
are Japanese. `notes-ja` retains historical planning notes and redirects.

The [feature ledger](../compatibility/features.json) owns status. English and
Japanese feature matrices are generated from that ledger, with identical IDs.
Do not use an old planning phase or translated narrative as a separate status
source.

## Use the prompts

Read the current design, roadmap, dependency policy and affected source contracts
before selecting a small end-to-end slice. Supply its feature IDs and actual
prerequisites to the [feature prompt](prompts/02-implement-feature.ja.md).

The [master prompt](prompts/00-master.ja.md) supplies shared guidance. The
[bootstrap prompt](prompts/01-bootstrap.ja.md) is retained for starting a project;
it is not an instruction to repeat this project's completed foundation.
Other prompts cover conformance, onboarding, resources and dependency updates.

## Kit provenance

The original archive contained 36 files, verified during setup. Its SHA-256 was
`450741ecb8376f1389cbeb0dae60539f574731857375746de6b05de495f7fabc`.
Its `project/` directory was installed at repository root; no duplicate project
is required. The source ZIP was removed after setup verification.

The imported API research baseline dates to **2026-10-21**. Refresh affected
official contracts and dependencies before implementation and release, following
the [dependency policy](../docs/en/dependency-policy.md). Historical research does
not establish current upstream behavior or executed compatibility.
