# Local container resource evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Status: PASS for the declared local Linux ARM64 container profile, measured on 2026-10-24 UTC. All five repetitions completed successfully with the frozen harness `ecb1430eddedcd6b63276bebc94a5f3885ce0393` and the fixed application source below.
The [measurement contract](../slices/dist-resource-measurement.md) records acceptance before implementation.
The application source selected for this local measurement is `bb8dd5cf9a2f9c61397981ef0591ed84b941c7ff`; publication-source selection remains pending.

## Result and supported scope

The [single final raw evidence set](dist-resource-measurement.txt) retains all 25 phase records, request quantiles, ten matching streamed SHA-256 downloads, five limit inspections, five clean exits, background-load snapshots, the unresolved freshness result and Rust checks. The window ran from 13:50:09 to 13:57:45 UTC, including fixture preparation and cleanup. It paused other workers' builds/tests but retained the four unrelated background containers.

Each fresh container completed 60 seconds idle, a 1 GiB JSON media upload/download and a 1 GiB JSON resumable upload/download at concurrency 1 with `memory.max=134217728`, `memory.swap.max=0`, `cpu.max=100000 100000` and `pids.max=64`. No transfer error or observed OOM/OOM-kill occurred. All final metadata sizes were exactly 1,073,741,824 bytes, every intermediate resumable PUT acknowledged the exact range, and all downloaded hashes matched `8c98dabd19d741c69da93af762702d9431b5dc832fb176230edf331a42e86919`. Targets stopped with exit 0 and OOMKilled=false; the harness removed all its containers, volumes and temporary fixtures. The same four background container IDs remained running afterward; only ordinal pseudonyms are retained here.

Values below use MiB = 1,048,576 bytes. Wall and CPU columns are medians of all five repetitions. Memory columns are the largest observed phase peaks across all five repetitions. Each transfer is exactly 1 GiB, so its CPU seconds and CPU seconds/GiB have the same numeric value.

| Phase | Wall s median | Process CPU s median | Cgroup CPU s median | Sampled RSS peak MiB max | Sampled cgroup current MiB max | Cumulative cgroup peak MiB max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Idle | 60.062609 | 0.00 | 0.000104 | 8.722656 | 3.914063 | 7.738281 |
| Media upload | 5.699540 | 4.43 | 4.434810 | 9.203125 | 128.660156 | 128.699219 |
| Media download | 3.688042 | 1.13 | 1.124922 | 9.417969 | 127.988281 | 128.699219 |
| Resumable upload | 8.904809 | 6.64 | 6.637044 | 9.621094 | 128.246094 | 128.699219 |
| Resumable download | 3.938409 | 1.10 | 1.102915 | 9.785156 | 127.941406 | 128.699219 |

All five idle process-CPU deltas were 0 at the 0.01-second tick resolution. Idle cgroup-CPU deltas were 0.000198, 0.000056, 0.000109, 0.000076 and 0.000104 seconds. These are separate counters, not proof of exactly zero CPU work. The observer took 50 ms sleeps plus probe overhead, producing approximately 15–18 samples/second in these phases; samples can miss transients. Kernel cgroup peaks are cumulative from creation and include earlier phases, unlike the sampled phase peaks.

The target limit was verified at every probe; it was never raised. The observed cumulative high-water mark reached **128.699219 MiB** despite a configured 128 MiB maximum. The official kernel documentation's [memory.max entry](https://docs.kernel.org/admin-guide/cgroup-v2.html#memory-interface-files) states: “Under certain circumstances, the usage may go over the limit temporarily.” The [retrieved-content digest](../slices/dist-resource-measurement.md#counter-references) and raw values are retained rather than clipping peaks to the setting; this run did not trace the specific kernel allocation responsible. File-cache samples reached 122.0625 MiB. RSS can also exceed an idle cgroup charge because shared resident/file pages and cgroup charge ownership are different accounting views. Do not subtract these non-atomic observations to derive private memory or total host memory.

Phase-end allocated disk was 1,048,868 KiB after each media download and 1,049,448–1,049,452 KiB after resumable download. This includes the 1 GiB object and catalog/WAL allocation, excludes the host client fixture and is not a temporary-disk high-water mark. The Docker VM reported cgroup v2, Linux 7.0.14-linuxkit, aarch64, 14 CPUs and 16,745,295,872 bytes RAM; Engine/client 29.9.2, overlayfs image driver and `stat -f` data-volume filesystem type `ext2/ext3`. Host curl was 8.7.1, reported as x86_64-apple-darwin26.0 on this ARM64 host. The actual client identity is preserved rather than assumed native.

Request p50/p95/p99/max and throughput for every repetition are in the raw evidence. Resumable uploads had 128 requests per repetition; their p99 was 0.047808–0.062422 seconds while finalization maxima were 2.090812–2.208350 seconds. Media and download phases have one request each, so their within-repetition quantiles coincide. Wall time/throughput include curl, observer boundary-command overhead and, for downloads, streamed host hashing/backpressure. They are end-to-end observations, not isolated server throughput or a comparative improvement.

Preparation checks:

```text
bash -n scripts/measure-container-resources.sh scripts/measure-resumable.sh
exit 0
bash scripts/measure-container-resources.sh --self-check
PASS: resource arithmetic, non-atomic samples, malformed time, OOM, boundary CPU regression, quantiles, advertised origin and cleanup self-checks
exit 0
shellcheck scripts/measure-container-resources.sh scripts/measure-resumable.sh
exit 0
```

Build and measure only in the agreed execution windows:

```sh
source_sha=bb8dd5cf9a2f9c61397981ef0591ed84b941c7ff
docker buildx build --platform linux/arm64 --load \
  --label "org.opencontainers.image.revision=$source_sha" \
  --metadata-file "$TMPDIR/gcs-mimic-resource-build.json" \
  --tag gcs-mimic:resource-measurement .
observer_image=$(awk '$1=="FROM" {print $2; exit}' Dockerfile)
docker pull --platform linux/arm64 "$observer_image"
bash scripts/measure-resumable.sh --container gcs-mimic:resource-measurement "$source_sha"
```

The local image revision label is checked against the supplied source SHA. Before building, compare the protected Docker/runtime inputs against that SHA; the harness repeats this check. Build metadata provides the local manifest digest separately from the image configuration ID. No registry push/pull of a gcs-mimic release is performed.

Known gates remain: unresolved distroless digest freshness, unselected public release source, public images/archives and unperformed native-platform verification. Native historical measurements are preserved in [resumable resources](resumable-upload-resources.txt); they are not container measurements or a same-profile regression baseline.

## Verified preparation

- Application source: `bb8dd5cf9a2f9c61397981ef0591ed84b941c7ff`, unchanged runtime/build inputs.
- One successful final ARM64 build using the command above. Local OCI index digest `sha256:cc34482ef5729125e0220d5d4d791bc06332d91d15d98c441a8b205139a7cd88`; platform manifest `sha256:2009929236f99a1bf5a8f89faa9233eb64148e5af506fac23919d8837fd7aec8`; image config `sha256:51ef22748edfe6f9c5952ca2e407b0e95e97fc51f442e44a5a4b352b93741c26`. Docker Engine's image ID is the index digest in this containerd image store. Its local RepoDigests entry does not establish registry publication.
- Rust builder/observer index: `rust:1.99.1-slim-trixie@sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6`; runtime: `gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf`; Dockerfile frontend: `sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e`.
- Actual builder compiler: rustc 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`, 2026-10-13), `aarch64-unknown-linux-gnu`, LLVM 23.1.1. Host compiler has the same version/hash, `aarch64-apple-darwin`; host Cargo 1.99.1 (`5f94df478`, 2026-08-27).
- Host: macOS 27.0.1 build 26A434, Apple M4 Pro / Mac16,11, 68,719,476,736 bytes RAM, 14 logical CPUs. Docker VM and filesystem details are emitted by the measurement harness.
- `just check`: exit 0, 778 tests in 38 groups, zero failed/ignored; includes fmt, Clippy, workspace tests, generated compatibility checks, shell syntax and offline release-script checks. No full SDK/container-guide run was performed.
- `cargo xtask dependencies check`: exit 1 at Unix timestamp 1792848596. Rust 1.99.1 and all direct crates were CURRENT. The known distroless digest update remained UPDATE_AVAILABLE (`sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e`); no dependency or image pin was changed.
- Markdown lint: exit 0, 0 issues. Compatibility render/check and ShellCheck: exit 0.

Preparation failures: the first Docker invocation was blocked by sandbox socket permissions before any build began; the authorized invocation above succeeded. The first Markdown lint invocation could not resolve the npm registry under the sandbox; the permission-enabled invocation succeeded. Host sysctl also needed read permission. These are environment failures, not transfer results. Review caught an advertised-origin mismatch before any container measurement; the committed startup self-check now verifies the host mapping and `--api-base-url` agree.

## Background-load accounting

Four unrelated containers were running during preparation and are retained. The agreed execution window pauses this Run's other worker builds/tests; it does not exclusively reserve the host. Before and after measurement, record read-only background-container CPU/memory snapshots with stable ordinal pseudonyms instead of user-specific names or IDs:

```sh
docker stats --no-stream --format '{{.ID}}|{{.CPUPerc}}|{{.MemUsage}}' |
  sort | awk -F '|' '{printf "background-%d CPU=%s memory=%s\n",NR,$2,$3}'
```

Those snapshots are observations, not continuous monitoring or proof that background load was constant. They do not add other processes to the target's process/cgroup counters and cannot remove host contention from wall time or latency.

## First container baseline calibration

There is no earlier equivalent container baseline to compare. For future runs of this exact profile, apply the shared-contract formula to all five raw values: median M, relative spread S = (max − min) / M, allowed fraction F = max(0.10, 2 × S), upper bound M × (1 + F). This table freezes that first-profile calibration; it reports no prior-regression pass. Calculations use unrounded raw values; the display rounds to six decimal places. Memory entries are **MiB**, time/CPU entries are seconds, and transfer CPU/GiB has the same bound because each transfer is 1 GiB. Throughput uses reciprocal wall seconds, so its lower bound is 1024 divided by the listed wall-time upper bound. Every later run must still enforce the original 128 MiB limit and correctness gates.

| Phase | Metric | Median M | Fraction F | Upper bound |
| --- | --- | ---: | ---: | ---: |
| idle | process CPU | 0 | PENDING | PENDING |
| idle | cgroup CPU | 0.000104 | PENDING | PENDING |
| idle | sampled RSS MiB | 8.710938 | 0.100000 | 9.582031 |
| idle | sampled cgroup current MiB | 3.406250 | 0.392202 | 4.742188 |
| media-download | wall | 3.688042 | 0.163840 | 4.292292 |
| media-download | process CPU | 1.130000 | 0.100000 | 1.243000 |
| media-download | cgroup CPU | 1.124922 | 0.100000 | 1.237414 |
| media-download | sampled RSS MiB | 9.406250 | 0.100000 | 10.346875 |
| media-download | sampled cgroup current MiB | 127.898438 | 0.100000 | 140.688281 |
| media-upload | wall | 5.699540 | 0.100000 | 6.269494 |
| media-upload | process CPU | 4.430000 | 0.112867 | 4.930000 |
| media-upload | cgroup CPU | 4.434810 | 0.114456 | 4.942400 |
| media-upload | sampled RSS MiB | 9.187500 | 0.100000 | 10.106250 |
| media-upload | sampled cgroup current MiB | 128.523438 | 0.100000 | 141.375781 |
| resumable-download | wall | 3.938409 | 0.320066 | 5.198961 |
| resumable-download | process CPU | 1.100000 | 0.127273 | 1.240000 |
| resumable-download | cgroup CPU | 1.102915 | 0.100000 | 1.213207 |
| resumable-download | sampled RSS MiB | 9.746094 | 0.100000 | 10.720703 |
| resumable-download | sampled cgroup current MiB | 127.851563 | 0.100000 | 140.636719 |
| resumable-upload | wall | 8.904809 | 0.179987 | 10.507560 |
| resumable-upload | process CPU | 6.640000 | 0.207831 | 8.020000 |
| resumable-upload | cgroup CPU | 6.637044 | 0.210124 | 8.031648 |
| resumable-upload | sampled RSS MiB | 9.601563 | 0.100000 | 10.561719 |
| resumable-upload | sampled cgroup current MiB | 128.230469 | 0.100000 | 141.053516 |

Idle CPU calibration remains PENDING: every process delta is zero and cgroup deltas are below the shared-contract 0.1-second calibration boundary, despite the finer cgroup display. Idle wall time is a prescribed wait, so workload throughput/request latency/disk allocation and CPU/GiB are inapplicable there. Transfer request quantiles, phase-end disk, file-cache and cumulative kernel peaks remain present as raw diagnostics; cumulative peaks are not phase-specific regression metrics. The broad idle-cgroup and resumable-download wall bounds expose variation in this shared host. Retain the outliers and use a quieter equivalent run before tightening them; these bounds do not establish acceptable product performance or authorize increasing the memory limit.

## Limits and remaining gates

This is the first container profile, not a repeat of the native shared-contract baseline: CPU limit, network, source/build graph, client and cache accounting differ. No regression pass against 60a4f03, cross-product advantage, full benchmark matrix or constant-memory proof for arbitrary sizes/concurrency is claimed. Only the named 1 GiB/concurrency-1 cases are measured. Native AMD64/Windows/macOS resource runs, a published image, a final release tag, cold cache, full host isolation, SDK/guide journeys, concurrency 8/16, disk-full/crash/power-loss and temporary-disk peaks were not exercised here. Existing historical native evidence is unchanged.

DESIGN-079's declared local resource scope is complete. The separate release/freshness/CI requirements retain their unresolved results: local success does not resolve the release source, distroless drift, public image/archives, latest-dependency candidate CI or complete pre-push. Revalidate the final selected release source/build inputs and platform under the publication contract.
