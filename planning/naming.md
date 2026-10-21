# Naming decision: gcs-mimic

The owner selected **gcs-mimic** on 2026-10-21. “Mimic” communicates imitation and emulation, making the project purpose easy to understand. Use the exact lowercase spelling for the project, repository slug and CLI binary. Registry availability and publication are separate tasks.

## Alternatives considered

| Candidate | Suggested slug / binary | Intended impression | Tradeoff |
| --- | --- | --- | --- |
| **BucketMint** | `bucketmint` | Compact, memorable bucket-oriented product | Add a GCS descriptor because the name alone does not identify the protocol |
| **GCS Rehearsal** | `gcs-rehearsal` | A place to rehearse cloud-storage application behavior locally | Clear purpose, slightly longer command |
| **GCS Forge** | `gcs-forge` | A practical development and test tool | Common naming vocabulary needs an availability check |
| **Cumulus GCS** | `cumulus-gcs` | Cloud association with a distinctive product identity | Cumulus is used in many other projects |
| **GCS Mimic** | `gcs-mimic` | Compatibility and emulation are immediately apparent | The name creates a strong expectation of behavioral fidelity |
| **BucketLab** | `bucketlab` | A small laboratory for object-storage experiments | Could be mistaken for a service-agnostic storage tool |
| **GCS Sprout** | `gcs-sprout` | Small footprint and incremental growth | Less explicit about emulation |
| **GCS Replica** | `gcs-replica` | Direct description of the compatibility goal | Might imply replication tooling or production durability |

**Selected:** `gcs-mimic`. Description: “A lightweight Google Cloud Storage emulator in Rust.” Resource efficiency is a design goal until measurements establish it. The alternatives above are decision history, not an open choice.

Before publishing, check GitHub, crates.io, the intended OCI registry and relevant naming rights. Do not claim affiliation with Google. This naming decision does not block architecture or inventory work.

Use `gcs-mimic` consistently. Configuration environment variables use the `GCS_MIMIC_` prefix. The initial source-built image is `gcs-mimic:dev`; a registry namespace is specified only when a real publication target exists.
