# Local release gates on the integrated source

## Local gates on the 0.19.1 source

Recorded **2026-11-12** on macOS (Apple Silicon) for commit `16314feaf0690de6fdac78c026a4616be730f452`, tree `f5e0a4ed732bd99827dbc940f1eb05520b44fb1e` (package version 0.19.1). The commit that adds this record changes only this document and the [0.19.1 release notes](../releases/0.19.1.md).

| Command | Exit | Summary line from the output |
| --- | --- | --- |
| `just pre-push` | 0 | `all stages passed in 23m07s`; library tests `2063 passed`; `it_process` `105 passed`; the Go, Ruby, Node.js, PHP, Python, Java, .NET and Rust SDK suites passed; seven container `PASS` lines on each of `linux/amd64` and `linux/arm64` |
| `cargo xtask dependencies check --scope release` | 0 | 41 `CURRENT` lines and no `UPDATE_AVAILABLE` |
| `just check-dependencies` | 0 | 129 `CURRENT` lines and no `UPDATE_AVAILABLE`; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE` |

`just pre-push` ran once as a complete invocation from a clean working tree of that commit. Both dependency checks ran on commit `2a64cbba`, its parent, which differs only in `scripts/test-check.sh`, no release input.

[Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/runs/43153179304) had failed on commit `45bf7b39`, the first commit of the 0.19.1 changes on `main`, and an earlier `just pre-push` on commit `2a64cbba` stopped in its first stage for the same reason: `scripts/test-check.sh` pins the cargo commands that `scripts/check.sh` runs, and `scripts/check-docs.sh` had gained the option-list tests without that list. Commit `16314fea` adds the command to the list; [Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/runs/43154795334) and [Documentation checks](https://github.com/shiroemons/gcs-mimic/actions/runs/43154794975) then succeeded on it.

## Published 0.19.0 source

Recorded **2026-11-12** for tag `v0.19.0`, commit `a20f9d8a33521b6ab5c303fd5e08542c10e2ae2d`, tree `07473309437608eb588de32803a4227738a481e5`. Remote results were read back from GitHub after publication; every run below names that commit:

| Gate | Result | Evidence |
| --- | --- | --- |
| Complete local `just pre-push` | Exit 0; 16m42s; library tests 2059 passed | On commit `b8aa76d4`, recorded below |
| Foundation checks | `success` | [Run 43130102366](https://github.com/shiroemons/gcs-mimic/actions/runs/43130102366) |
| Documentation checks | `success` | [Run 43130102374](https://github.com/shiroemons/gcs-mimic/actions/runs/43130102374) |
| Release binaries dry run | `success`; all eight native build and test targets passed | [Run 43130114091](https://github.com/shiroemons/gcs-mimic/actions/runs/43130114091) |
| Tag publication | `success`; validate, publish, both container architectures, manifest and Docker Hub mirror passed | [Run 43135431922](https://github.com/shiroemons/gcs-mimic/actions/runs/43135431922) |

The tag workflow reused the successful dry-run binaries, so its `foundation`, `test` and `build` jobs were skipped rather than rerun. Its `validate` job passed the release-scoped dependency freshness check, which also passed locally just before the tag was created. The [published Release](https://github.com/shiroemons/gcs-mimic/releases/tag/v0.19.0) has eight archives, `SHA256SUMS` and `CONTAINER-DIGESTS.json`, whose `source_commit` is the tagged commit. The public multi-architecture image index is `sha256:9b0f0c0a276b4d16684a0cfa6b07ad87755be91c025c1e7d16139ed716e3d4d1`, with `sha256:b664cea96b7582d03a547d36fcacb67666b2d661f9c6895602838958b99b99ef` for `linux/amd64` and `sha256:ff9dba97a87a92050a8c0ff6a4221b4aaac34d6fd71abe8612da240c0c2ee1a1` for `linux/arm64`. GHCR `0.19.0`, `0.19` and `latest` and Docker Hub `0.19.0` and `latest` resolved to that index, and the GHCR image printed `gcs-mimic 0.19.0`.

### Local gates on the 0.19.0 source

Recorded on macOS (Apple Silicon) for commit `b8aa76d46fc4aebd26502a98511e4942c5cd5f92`, tree `7e64f557afdc4f2df8b5e8290b9d15ff93816e44` (package version 0.19.0). That commit adopts BuildKit v0.35.0 after the first tag publication stopped, as described under [First 0.19.0 tag](#first-0190-tag). The tagged commit changes only this document and the [0.19.0 release notes](../releases/0.19.0.md), so it touches none of the protected paths listed under [Source relation](#source-relation).

| Command | Exit | Summary line from the output |
| --- | --- | --- |
| `just pre-push` | 0 | `all stages passed in 16m42s`; library tests `2059 passed`; `it_process` `102 passed`; every SDK case `PASS`; seven container `PASS` lines on each of `linux/amd64` and `linux/arm64`, including the strict local recipe and the full application example without `GCS_MIMIC_SCHEME` |
| `cargo xtask dependencies check --scope release` | 0 | 41 `CURRENT` lines, among them `docker moby/buildkit:v0.35.0: CURRENT` and `docker moby/buildkit:latest: CURRENT` |
| `cargo xtask dependencies check` | 1 | 120 `CURRENT` lines; every `UPDATE_AVAILABLE` line names a development-only input that ships in no artifact, listed in the [2026-11-12 refresh](dependency-refresh.md#refresh-2026-11-12); fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE` |

`just pre-push` ran once as a complete invocation from a clean working tree of that commit. The release-scoped check is the freshness gate the release workflow's `validate` job applies; the full check is what `just check-dependencies` runs, and under the [release-scope rule](../dependency-policy.md#ci-lanes) its development-only updates do not block publication. Both dependency checks ran on the working tree just before the commit, when the workflow pin and the snapshot's BuildKit entry already matched it; the snapshot's summary fields and the refresh evidence were then written from their output.

### First 0.19.0 tag

The first `v0.19.0` tag named commit `364a6d36e28646f3225c51ad8ff905d1efcce762`, which recorded these gates on commit `95c4769c993c3529e5d272462dec86c96bb4d485`, tree `aa8ac0d7767f44681746540ce969499e1714f73d`, from a clean working tree:

| Command | Exit | Summary line from the output |
| --- | --- | --- |
| `just check-dependencies` | 0 | 129 `CURRENT` lines; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE` |
| `just pre-push` | 0 | `all stages passed in 24m22s`; library tests `2059 passed`; `it_process` `102 passed`; every SDK case `PASS`; seven container `PASS` lines on each of `linux/amd64` and `linux/arm64` |

The wrapper that recorded that exit status failed after `just pre-push` returned, because zsh reserves the name `status`; the exit above is inferred from the final progress line, which the recipe prints only after its last stage succeeds. An earlier `just pre-push` on commit `cb2667f9` was stopped before its cross-target stage, after the first CI run of the feature showed that `x86_64-pc-windows-msvc` Clippy rejected an unused Unix-only method; commit `95c4769c` fixed it and `bash scripts/check-cross.sh` then passed for the Windows, Linux glibc and Linux musl targets.

[Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/runs/43038868679) and the [Release binaries dry run](https://github.com/shiroemons/gcs-mimic/actions/runs/43038869036) succeeded on that commit once their musl jobs were rerun; the earlier attempts failed only because the runners could not resolve the Ubuntu archive hosts during an outage of Canonical's DNS. The [tag publication](https://github.com/shiroemons/gcs-mimic/actions/runs/43126574525) then stopped in `validate`, before any job that publishes: BuildKit v0.35.0 had been released after the dry run, and the release-scoped check reported the pinned v0.34.2 as outdated. No Release, image or archive was published from that tag. It was then deleted from GitHub and `v0.19.0` was created again on commit `a20f9d8a`, so the tag that published is the only one users could have received with release artifacts.

## Published 0.18.0 source

Recorded **2026-11-11** for tag `v0.18.0`, commit `7b4a8006ddb38caf87ed43f8881953dbda348605`, tree `f39350928070b35dc92088d3219bbeb98227e1f9`. After the earlier local gates below, the recording commit changed documents and the ledger, and commit `7b4a8006` changed only Windows process-test expectations in `tests/it_process/foundation.rs`. The complete `just pre-push` was rerun on the tagged tree and exited 0 in **16m03s**; its library tests reported **2052 passed**. This final-source run supersedes the earlier pre-push result. The earlier freshness, Markdown and compatibility results retain their original source identities below; they are not relabeled as tagged-tree runs.

Remote results were read back from GitHub after publication; every run below names commit `7b4a8006ddb38caf87ed43f8881953dbda348605` and therefore tree `f39350928070b35dc92088d3219bbeb98227e1f9`:

| Gate | Result | Evidence |
| --- | --- | --- |
| Complete local `just pre-push` | Exit 0; 16m03s; library tests 2052 passed | Final-source rerun recorded above |
| Foundation checks | `success` | [Run 43003842704](https://github.com/shiroemons/gcs-mimic/actions/runs/43003842704) |
| Release binaries dry run | `success`; all eight native build and test targets passed | [Run 43003843569](https://github.com/shiroemons/gcs-mimic/actions/runs/43003843569) |
| Tag publication | `success`; validate, publish, both container architectures, manifest and Docker Hub mirror passed | [Run 43008744768](https://github.com/shiroemons/gcs-mimic/actions/runs/43008744768) |

The tag workflow reused the successful dry-run binaries, so its `foundation`, `test` and `build` jobs were skipped rather than rerun. Its `validate` job passed the release-scoped dependency freshness check. The [published Release](https://github.com/shiroemons/gcs-mimic/releases/tag/v0.18.0) has eight archives, `SHA256SUMS` and `CONTAINER-DIGESTS.json`. The public multi-architecture image index is `sha256:ed8328f03202da4300b48eea94ac4eeceadbbda5800f39a20abb2660ff66c7b9` for `linux/amd64` and `linux/arm64`.

## Earlier local gates on the 0.18.0 source

Recorded **2026-11-11** before the Windows test correction. `just pre-push` and `just check-dependencies` each ran once, from a clean working tree whose content is tree `7db8ab942ba49db8037fa8fa1499909f089f3e6f` (package version 0.18.0). The commit with exactly that tree is `1e5a89a042ef4936da3a99518d81c957720b7fef`. `just check-markdown` and `cargo xtask compatibility check` ran on tree `a5dbbf462cddbdaa09e66def0c2c87bc46b38985` of recording commit `4264cdde`, which differs from that source only in documents and the ledger. Each gate passed in a single complete invocation; no result below is assembled from separately run stages. The two failed `just pre-push` attempts that preceded the passing run are recorded under [Earlier attempts on the 0.18.0 source](#earlier-attempts-on-the-0180-source). The earlier set for 0.17.1 is kept under [Earlier set on the 0.17.1 source](#earlier-set-on-the-0171-source), and the failures recorded on 2026-10-24 in the [initial release preparation evidence](dist-release-notes.md#historical-pre-tag-execution) remain as history.

### Source relation

`just pre-push` and `just check-dependencies` did not run on the commit that records them. That commit changes only this document, the [0.18.0 release notes](../releases/0.18.0.md) and `compatibility/features.json`. Regenerating `docs/en/feature-matrix.md` and `docs/ja/feature-matrix.md` changed neither view, because both rows keep status `PARTIAL`. The commit touches no protected path, so the following comparison is empty for it:

```sh
git diff --name-only 1e5a89a042ef4936da3a99518d81c957720b7fef <recording commit> -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
```

Any later change to those paths needs the gates to run again on that source.

### Commands and results

| Command | Exit | Summary line from the output |
| --- | --- | --- |
| `just check-dependencies` | 0 | 129 `CURRENT` lines; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE` |
| `just check-markdown` | 0 | `PASS: 388 Markdown files and 2 generated views have no lint issues` |
| `cargo xtask compatibility check` | 0 | `Validated 607 feature rows; 401 parameter locations and 357 schema fields are separate inventories.` |
| `just pre-push` | 0 | `[pre-push 3/3 100%] container checks took 9m41s; all stages passed in 26m17s` |

### Dependency freshness

`just check-dependencies` runs `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` over every declared scope (`scope=All`). It confirmed that the release platforms `linux/amd64` and `linux/arm64` match the ledger and printed 129 lines with status `CURRENT`, among them `rust: CURRENT current=1.99.1 latest=1.99.1`, `Go toolchain: CURRENT current=1.27.4 latest=1.27.4` and `Ruby toolchain: CURRENT current=4.0.9 latest=4.0.9`. No input reported an available update. `docker library/composer:latest` reported `CURRENT on declared platforms linux/amd64, linux/arm64` followed by `INFO: latest differs only on linux/riscv64`, a platform outside the release platforms. The only other status line is the fixed migration baseline that the [dependency policy](../dependency-policy.md#daily-update-detection-and-bounded-upgrade-work) prints as `PINNED_REFERENCE` rather than `CURRENT`:

```text
github.com/fsouza/fake-gcs-server: PINNED_REFERENCE current=v1.56.1 latest=v1.56.1; migration contract is revision 896ece51e49eed0946089f3089f6343855c0d106
```

Its pinned version equals the latest release. No exception was used.

### Markdown lint

`just check-markdown` runs `bash scripts/check-markdown.sh`, which lints every tracked Markdown file with markdownlint-cli2 v0.24.0 (markdownlint v0.42.0). Both runs reported `Summary: 0 issues in 0 files`: 388 prose files under the repository configuration, then the two generated feature matrices under `docs/generated-views.markdownlint-cli2.jsonc`.

The recorded run is on the content of the recording commit. An earlier run by the same command, on the 0.18.0 content before the commits that removed one test import, changed two request handlers with their tests, changed one sync test and added the [request deadline evidence](request-deadline-load.md), also exited 0 with 387 prose files and 2 generated views and no issues. The added evidence document accounts for the difference of one file.

### Compatibility check

`cargo xtask compatibility check` validated the ledger and confirmed that both generated feature matrices match it.

### Full pre-push

`just pre-push` ran its three stages in order in one invocation. The stage lines from the output:

```text
[pre-push 1/3 done] Rust and shell checks took 15m42s. [pre-push 2/3 61%] SDK suites started; about 9m16s left
[pre-push 2/3 done] SDK suites took 54s. [pre-push 3/3 65%] container checks started; about 8m24s left
[pre-push 3/3 100%] container checks took 9m41s; all stages passed in 26m17s
```

- **Rust and shell checks** (`just check`): the shell self-tests passed, including `bash scripts/test-check.sh` with `PASS: shared checks run in order, run every test binary, and stop at every failing command`; that script injects a failing Cargo command at each step and requires the shared check to stop there. The production lint reported no new violation for the host and for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`. The Rust test run ended with `PASS: 16 test binaries and doctests with 1 concurrent binaries`; the library tests reported `2052 passed; 0 failed`, and the 16 per-target `test result: ok.` lines sum to 3386 passed, 0 failed and 1 ignored (in `it_admin`). The stage also ran the compatibility check (607 rows) and the release, recipe, publication and SDK-runner self-checks.
- **SDK suites** (`just check-sdk`): Go (`ok gcs-mimic/sdk-tests/go`), Ruby with the pinned 4.0.9, Node.js, Python, PHP, Java, .NET and Rust. The .NET suite ran on both target frameworks, reporting `dotnet-runtime: .NET 10.0.15` and `dotnet-runtime: .NET 11.0.0`, and ended with `PASS: .NET SDK suite on net10.0 and net11.0`.
- **Container checks** (`just check-containers`): `linux/arm64` and `linux/amd64` each built the image and passed every smoke scenario, from `PASS: <platform> build, isolated health, host/DNS access, bucket/object media CRUD, stop/start and down/up persistence, deliberate volume reset, resource limits, SIGINT and SIGTERM` through the full application example.

## Earlier attempts on the 0.18.0 source

Two earlier `just pre-push` invocations on this line of work failed in the Rust and shell stage, each with exit 101, and were fixed before the passing run. They are history, not passes.

- The first stopped at the warning-free check for a non-Unix target: `error: unused import: CleanEnv` in `tests/it_process/option_aliases.rs`. The commit `test(cli): import the clean environment helper only on Unix` fixed it.
- The second reported `test result: FAILED. 2048 passed; 1 failed` for the library tests. `server::batch::tests::a_part_past_the_request_limit_answers_alone_and_later_parts_still_run` failed at `assertion failed: next_text(&mut body).await.contains("204")`: a DELETE response was replaced by a request timeout while its catalog commit was still running. The commit `fix(server): preserve delete responses during catalog waits` fixed it; the [request deadline evidence](request-deadline-load.md) records the diagnosis.

## Earlier set on the 0.17.1 source

On the same day every gate also ran once on tree `17a2db8c7db85b5b83feaf8757bec935acd8bf65` (package version 0.17.1), the integrated lifecycle-transition source; the commit on `main` with exactly that tree is `accb532bc9da7282437ae0aad71cd0545a93cb60`. That set is superseded by the 0.18.0 results above and kept because ledger records cite it.

| Command | Exit | Summary line from the output |
| --- | --- | --- |
| `just check-dependencies` | 0 | 129 `CURRENT` lines; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE` |
| `just check-markdown` | 0 | `PASS: 384 Markdown files and 2 generated views have no lint issues` |
| `cargo xtask compatibility check` | 0 | `Validated 607 feature rows; 401 parameter locations and 357 schema fields are separate inventories.` |
| `just pre-push` | 0 | `[pre-push 3/3 100%] container checks took 8m24s; all stages passed in 24m16s` |

In that run the Rust and shell checks took 15m00s and the SDK suites 52s; the per-target `test result: ok.` lines summed to 3357 passed, 0 failed and 1 ignored. Its recording commits changed only documents and the ledger, and the protected-path comparison against `accb532bc9da7282437ae0aad71cd0545a93cb60` was empty for each of them.

## Ledger effect

- DIST-002 `DIST-002-LOCAL-GATES` (`release-local-gates`) passes for the 0.18.0 source: the earlier freshness, Markdown and compatibility checks passed on their identified trees, the complete `just pre-push` exited 0 on tagged tree `f39350928070b35dc92088d3219bbeb98227e1f9`, and tag validation passed release-scoped freshness. DIST-002 stays `PARTIAL` because its pin-revision, pin-diff and tag-freshness contract cases are still pending.
- DESIGN-099 `DESIGN-099-local-checks` (`shared-check-failure-propagation`, `local-pre-push-suite`) passes. DESIGN-099 stays `PARTIAL`: every acceptance case passes, but the row keeps its failed 2026-10-24 verification records as history, and `cargo xtask compatibility` rejects an `IMPLEMENTED` row with any failed verification.

## Scope and limitations

The earlier local runs used local test images. The published-source section separately records the completed remote CI, native release matrix and tag workflows. This follow-up only reads those results and the public assets; it does not push, dispatch workflows or publish anything. Both ledger rows retain their statuses and case results, including historical failed verification records.
