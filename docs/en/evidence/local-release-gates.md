# Local release gates on the integrated source

Recorded **2026-11-11** for **0.18.0**. `just pre-push` and `just check-dependencies` each ran once, from a clean working tree whose content is tree `7db8ab942ba49db8037fa8fa1499909f089f3e6f` (package version 0.18.0). The commit with exactly that tree is `1e5a89a042ef4936da3a99518d81c957720b7fef`. `just check-markdown` and `cargo xtask compatibility check` ran on the content of the commit that records these results, which differs from that source only in documents and the ledger. Each gate passed in a single complete invocation; no result below is assembled from separately run stages. The two failed `just pre-push` attempts that preceded the passing run are recorded under [Earlier attempts on the 0.18.0 source](#earlier-attempts-on-the-0180-source). The earlier set for 0.17.1 is kept under [Earlier set on the 0.17.1 source](#earlier-set-on-the-0171-source), and the failures recorded on 2026-10-24 in the [initial release preparation evidence](dist-release-notes.md#historical-pre-tag-execution) remain as history.

## Source relation

`just pre-push` and `just check-dependencies` did not run on the commit that records them. That commit changes only this document, the [0.18.0 release notes](../releases/0.18.0.md) and `compatibility/features.json`. Regenerating `docs/en/feature-matrix.md` and `docs/ja/feature-matrix.md` changed neither view, because both rows keep status `PARTIAL`. The commit touches no protected path, so the following comparison is empty for it:

```sh
git diff --name-only 1e5a89a042ef4936da3a99518d81c957720b7fef <recording commit> -- \
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
| `just check-markdown` | 0 | `PASS: 388 Markdown files and 2 generated views have no lint issues` |
| `cargo xtask compatibility check` | 0 | `Validated 607 feature rows; 401 parameter locations and 357 schema fields are separate inventories.` |
| `just pre-push` | 0 | `[pre-push 3/3 100%] container checks took 9m41s; all stages passed in 26m17s` |

## Dependency freshness

`just check-dependencies` runs `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` over every declared scope (`scope=All`). It confirmed that the release platforms `linux/amd64` and `linux/arm64` match the ledger and printed 129 lines with status `CURRENT`, among them `rust: CURRENT current=1.99.1 latest=1.99.1`, `Go toolchain: CURRENT current=1.27.4 latest=1.27.4` and `Ruby toolchain: CURRENT current=4.0.9 latest=4.0.9`. No input reported an available update. `docker library/composer:latest` reported `CURRENT on declared platforms linux/amd64, linux/arm64` followed by `INFO: latest differs only on linux/riscv64`, a platform outside the release platforms. The only other status line is the fixed migration baseline that the [dependency policy](../dependency-policy.md#daily-update-detection-and-bounded-upgrade-work) prints as `PINNED_REFERENCE` rather than `CURRENT`:

```text
github.com/fsouza/fake-gcs-server: PINNED_REFERENCE current=v1.56.1 latest=v1.56.1; migration contract is revision 896ece51e49eed0946089f3089f6343855c0d106
```

Its pinned version equals the latest release. No exception was used.

## Markdown lint

`just check-markdown` runs `bash scripts/check-markdown.sh`, which lints every tracked Markdown file with markdownlint-cli2 v0.24.0 (markdownlint v0.42.0). Both runs reported `Summary: 0 issues in 0 files`: 388 prose files under the repository configuration, then the two generated feature matrices under `docs/generated-views.markdownlint-cli2.jsonc`.

The recorded run is on the content of the recording commit. An earlier run by the same command, on the 0.18.0 content before the commits that removed one test import, changed two request handlers with their tests, changed one sync test and added the [request deadline evidence](request-deadline-load.md), also exited 0 with 387 prose files and 2 generated views and no issues. The added evidence document accounts for the difference of one file.

## Compatibility check

`cargo xtask compatibility check` validated the ledger and confirmed that both generated feature matrices match it.

## Full pre-push

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

- DIST-002 `DIST-002-LOCAL-GATES` (`release-local-gates`) passes for the 0.18.0 source: freshness exited 0 with every checked input `CURRENT` apart from the pinned migration baseline, Markdown lint reported zero issues, the complete `just pre-push` exited 0 on the final 0.18.0 source and the compatibility check exited 0. DIST-002 stays `PARTIAL` because its pin-revision, pin-diff and tag-freshness contract cases are still pending.
- DESIGN-099 `DESIGN-099-local-checks` (`shared-check-failure-propagation`, `local-pre-push-suite`) passes. DESIGN-099 stays `PARTIAL`: every acceptance case passes, but the row keeps its failed 2026-10-24 verification records as history, and `cargo xtask compatibility` rejects an `IMPLEMENTED` row with any failed verification.

## Scope and limitations

No tag, push, workflow dispatch, release or image publication was performed. The container images were local test images. Remote CI and the tag workflows have not run for 0.18.0. Remote CI evidence for DESIGN-099 remains the earlier recorded runs; these local results do not replace remote execution or the native release-matrix archives.
