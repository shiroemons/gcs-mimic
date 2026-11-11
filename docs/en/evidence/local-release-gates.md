# Local release gates on the integrated source

Recorded **2026-11-11**. Every local release gate ran once, from a clean working tree whose content is tree `17a2db8c7db85b5b83feaf8757bec935acd8bf65` (package version 0.17.1), the integrated lifecycle-transition source. The commit on `main` with exactly that tree is `accb532bc9da7282437ae0aad71cd0545a93cb60`. This document named a different commit hash for the same tree until the unpublished history was tidied without changing file contents; the gates did not run again. Each gate passed in a single complete invocation; no result below is assembled from separately run stages. The failures recorded on 2026-10-24 in the [initial release preparation evidence](dist-release-notes.md#historical-pre-tag-execution) remain as history; these results are a later, separate execution on newer source.

## Source relation

The gates did not run on the commits that record them. The commit that recorded the results changes only this document, a pointer in [initial release preparation evidence](dist-release-notes.md), a pointer in [pre-push verification](pre-push.md) and `compatibility/features.json`; the later commit that updated the source commit hash changes only documents and the ledger. Regenerating `docs/en/feature-matrix.md` and `docs/ja/feature-matrix.md` changed neither view, because both rows keep status `PARTIAL`. Neither commit touches a protected path, so the following comparison is empty for each of them:

```sh
git diff --name-only accb532bc9da7282437ae0aad71cd0545a93cb60 <recording commit> -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
```

Any later change to those paths needs the gates to run again on that source.

## Commands and results

| Command | Exit | Summary line from the output |
| --- | --- | --- |
| `just check-dependencies` | 0 | 129 `CURRENT` lines; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE` |
| `just check-markdown` | 0 | `PASS: 384 Markdown files and 2 generated views have no lint issues` |
| `cargo xtask compatibility check` | 0 | `Validated 607 feature rows; 401 parameter locations and 357 schema fields are separate inventories.` |
| `just pre-push` | 0 | `[pre-push 3/3 100%] container checks took 8m24s; all stages passed in 24m16s` |

## Dependency freshness

`just check-dependencies` runs `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` over every declared scope (`scope=All`). It confirmed that the release platforms `linux/amd64` and `linux/arm64` match the ledger and printed 129 lines with status `CURRENT`, among them `rust: CURRENT current=1.99.1 latest=1.99.1`, `Go toolchain: CURRENT current=1.27.4 latest=1.27.4` and `Ruby toolchain: CURRENT current=4.0.9 latest=4.0.9`. No input reported an available update. The only other status line is the fixed migration baseline that the [dependency policy](../dependency-policy.md#daily-update-detection-and-bounded-upgrade-work) prints as `PINNED_REFERENCE` rather than `CURRENT`:

```text
github.com/fsouza/fake-gcs-server: PINNED_REFERENCE current=v1.56.1 latest=v1.56.1; migration contract is revision 896ece51e49eed0946089f3089f6343855c0d106
```

Its pinned version equals the latest release. No exception was used.

## Markdown lint

`just check-markdown` runs `bash scripts/check-markdown.sh`, which lints every tracked Markdown file with markdownlint-cli2 v0.24.0 (markdownlint v0.42.0). Both runs reported `Summary: 0 issues in 0 files`: 384 prose files under the repository configuration, then the two generated feature matrices under `docs/generated-views.markdownlint-cli2.jsonc`.

## Compatibility check

`cargo xtask compatibility check` validated the ledger and confirmed that both generated feature matrices match it.

## Full pre-push

`just pre-push` ran its three stages in order in one invocation. The stage lines from the output:

```text
[pre-push 1/3 done] Rust and shell checks took 15m00s. [pre-push 2/3 33%] SDK suites started
[pre-push 2/3 done] SDK suites took 52s. [pre-push 3/3 66%] container checks started
[pre-push 3/3 100%] container checks took 8m24s; all stages passed in 24m16s
```

- **Rust and shell checks** (`just check`): the shell self-tests passed, including `bash scripts/test-check.sh` with `PASS: shared checks run in order, run every test binary, and stop at every failing command`; that script injects a failing Cargo command at each step and requires the shared check to stop there. The Rust test run ended with `PASS: 16 test binaries and doctests with 1 concurrent binaries`; its per-target `test result: ok.` lines sum to 3357 passed, 0 failed and 1 ignored (in `it_admin`). The stage also ran the compatibility check (607 rows) and the release, recipe, publication and SDK-runner self-checks.
- **SDK suites** (`just check-sdk`): Go (`ok gcs-mimic/sdk-tests/go`), Ruby with the pinned 4.0.9, Node.js, Python, PHP, Java, .NET and Rust. The .NET suite ran on both target frameworks, reporting `dotnet-runtime: .NET 10.0.15` and `dotnet-runtime: .NET 11.0.0`, and ended with `PASS: .NET SDK suite on net10.0 and net11.0`.
- **Container checks** (`just check-containers`): `linux/arm64` and `linux/amd64` each built the image and passed every smoke scenario, from `PASS: <platform> build, isolated health, host/DNS access, bucket/object media CRUD, stop/start and down/up persistence, deliberate volume reset, resource limits, SIGINT and SIGTERM` through the full application example.

## Ledger effect

- DIST-002 `DIST-002-LOCAL-GATES` (`release-local-gates`) passes: freshness exited 0 with every checked input `CURRENT` apart from the pinned migration baseline, Markdown lint reported zero issues, the complete `just pre-push` exited 0 on the final integrated source and the compatibility check exited 0. DIST-002 stays `PARTIAL` because its pin-revision, pin-diff and tag-freshness contract cases are still pending.
- DESIGN-099 `DESIGN-099-local-checks` (`shared-check-failure-propagation`, `local-pre-push-suite`) passes. DESIGN-099 stays `PARTIAL`: every acceptance case passes, but the row keeps its failed 2026-10-24 verification records as history, and `cargo xtask compatibility` rejects an `IMPLEMENTED` row with any failed verification.

## Scope and limitations

No tag, push, workflow dispatch, release or image publication was performed. The container images were local test images. Remote CI evidence for DESIGN-099 remains the earlier recorded runs; these local results do not replace remote execution or the native release-matrix archives.
