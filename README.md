# gcs-mimic

A lightweight Google Cloud Storage emulator in Rust.

**Foundation stage: compatibility-ledger tooling is available. The storage server, Docker image, fixture importer and SDK examples are not implemented yet.** Resource efficiency is a design goal, not a measured result. The selected project name and future binary are `gcs-mimic`.

## Contributor quick start

Prerequisites: Git, rustup and Python 3.11+ for dependency checks. The pinned compiler is Rust 1.99.1, edition 2024. No Google credentials or cloud project are needed.

```sh
git clone https://github.com/shiroemons/gcs-mimic.git
cd gcs-mimic
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo +1.99.1 fetch --locked
python3 scripts/check-dependencies.py
cargo +1.99.1 xtask compatibility validate
cargo +1.99.1 xtask compatibility render
cargo +1.99.1 xtask compatibility check
cargo +1.99.1 test --workspace --locked --offline
cargo +1.99.1 fmt --all --check
cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings
python3 -m unittest discover -s scripts -p 'test_*.py'
```

The `cargo xtask` alias uses the lockfile and offline resolution. Run `cargo fetch --locked` once before offline checks. Explicit `+1.99.1` overrides a conflicting `RUSTUP_TOOLCHAIN` environment variable; ordinary `cargo` uses `rust-toolchain.toml` when no environment override exists.

Run the read-only dependency check at each slice start and before release. It compares official Rust metadata, Cargo registry releases (including new majors) and the pinned GitHub Action. Exit 1 means updates are available; exit 2 means the check failed. Daily detection is configured for every calendar day with no cooldown; activation requires the workflow to reach the default branch. Scheduled/remote CI execution has not been verified yet.

`compatibility/features.json` is the only feature-status authority. English and Japanese tables are generated together. See the [ledger workflow](docs/en/compatibility-policy.md), [dependency snapshot](compatibility/dependency-status.json) and [verification evidence](docs/en/evidence/ledger-bootstrap.txt). Supplementary kit material is organized under [planning/](planning/README.md); canonical project files are at repository root.

## Planned experience

Build and run a local Docker service, seed fixture buckets from a read-only directory, persist changes in a separate volume, connect official SDKs, and exercise private/public/signed object access. Use a permissive development profile for convenient startup and a strict local-identity profile for authorization and signature tests.

See the [proposed getting-started contract](docs/en/getting-started-spec.md) for the full installation-to-CI journey. Its configuration, initial data, Docker build/start, health, CRUD, SDK, public/signed URL, persistence, stop/reset and isolated CI commands are future acceptance targets. No container base image or digest has been selected; DEP-008 remains pending.

## Project documents

- [Design](docs/en/design.md)
- [Incremental roadmap](docs/en/roadmap.md)
- [Compatibility policy](docs/en/compatibility-policy.md)
- [Complete feature matrix](docs/en/feature-matrix.md)
- [API inventory](docs/en/api-inventory.md)
- [Source provenance and known gaps](docs/en/sources.md)
- [Latest stable dependency policy](docs/en/dependency-policy.md)
- [Machine-readable feature ledger](compatibility/features.json)
- [Known specification gaps](compatibility/gaps.json)
- [Coding-agent rules](AGENTS.md)

## Implementation principles

Use bounded streaming with disk-backed data, transactional metadata, atomic generation conditions and explicit crash recovery. Share domain semantics across JSON, XML and gRPC adapters. Keep cloud infrastructure simulations clearly distinguished from exact local wire behavior.

Adopt the latest stable Rust and chosen dependencies, including intentional major-version migrations. Record verified versions in the toolchain file, lockfiles and image digests. Refresh them continuously through tested updates.

Code, comments, commit messages, CLI output and canonical documentation are English. The owner's implementation prompts and work reports are Japanese. Implement one small feature slice and update evidence-backed status at a time.

This is an independent emulator project. It is not an official Google product.
