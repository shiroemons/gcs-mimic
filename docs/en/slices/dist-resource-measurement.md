# Local container resource measurement

<!-- markdownlint-configure-file {"MD013": false} -->

## Scope and fixed inputs

Measure application source `bb8dd5cf9a2f9c61397981ef0591ed84b941c7ff` (catalog v14, rusqlite 0.41.0) using the unchanged Dockerfile and locked build graph. This is local Distribution evidence for DESIGN-079 and the resource portion of release verification, not selection or approval of a release source, registry publication, native AMD64 evidence, or dependency freshness completion. DIST-001/002/003, DESIGN-059/099 and DEP-005/006/009 retain their existing unrelated results and gates.

Reuse the disk fixture, curl streaming, response-size/range assertions and streamed download SHA-256 checks from `scripts/measure-resumable.sh`. Preserve its historical native evidence. `--container IMAGE SOURCE_SHA` selects the new container harness and never builds or publishes an image. Build the final image once before the coordinator-approved exclusive measurement window.

## Acceptance cases recorded before implementation

| Case | Required observable result |
| --- | --- |
| container-resource-self-check | Synthetic probe records prove independent RSS/cgroup values, CPU deltas, byte units, phase filtering and failure on missing/invalid time/counters or regressing boundary CPU. Concurrent sample reordering and separate cache/current reads must not cause false failures. Stubbed Docker startup verifies published host port and advertised API origin agree; cleanup mocks verify all actions run and the original failure survives; diagnostic filtering retains safe curl/HTTP codes. Syntax and ShellCheck pass. |
| container-resource-idle | Five fresh isolated containers, each measured idle for 60 seconds after readiness. Record process RSS, cgroup current/peak/cache memory, process and cgroup CPU seconds, wall time and errors separately. |
| container-resource-transfer | Each repetition uploads and downloads exactly 1 GiB at concurrency 1 through JSON media and then resumable upload; require exact final size and streamed SHA-256, all intermediate 308 acknowledged ranges, and zero transfer errors. Record each direction's CPU seconds/GiB, wall time, throughput, request p50/p95/p99 and phase-end disk allocation. |
| container-resource-limit | Inspect and verify 128 MiB memory, zero swap, one CPU and 64 PIDs. Confirm cgroup limits directly, no OOM/OOM-kill events, server still ready and clean SIGTERM exit. RSS is never substituted for cgroup memory. |
| container-resource-provenance | Record source SHA, Dockerfile/lock/toolchain/harness hashes, local image ID and manifest digest, base digests, observer image digest, host/VM/tools/filesystem/cache/concurrency/auth/durability conditions and all five raw results including outliers. Record failures and unperformed checks explicitly. |

## Measurement contract

The shared-contract `transfer-1gib` names media upload/download, correctness, five repetitions, separated generator resources, CPU/GiB, RSS, latency, disk and observational sampling. This slice adds a **new container profile**, including the existing resumable method. It does not compare its values with the native 60a4f03 baseline: source, network, CPU limit, cgroup/page-cache accounting and client differ. No native regression gate or performance improvement can be inferred. Synchronization/startup/publication benchmarks remain owned by their existing slices.

Run the image as non-root with a read-only root filesystem and a fresh named disk volume per repetition. Use HTTP/1, permissive/native defaults, SQLite WAL/FULL, two async and two blocking workers, one client, a 2 GiB object limit, 128 MiB memory, no swap, one CPU and 64 PIDs. An 8 MiB random disk chunk is repeated to create a 1 GiB disk-backed client fixture; curl streams files and downloads into SHA-256. No compression or cache eviction is requested. Sequential warm/mixed caches are retained, including the first repetition.

A separate observer uses the already pinned Rust builder image, shares only the target PID namespace, reads cgroup v2 from a read-only host cgroup mount, and reads the data volume read-only for phase-end disk allocation. Its CPU/memory and the host curl/hash clients are outside the target cgroup. It needs no privileged container, Docker socket mount, runtime shell addition or application source changes. Read target `/proc/1/status` RSS and `/proc/1/stat` CPU ticks independently of `memory.current`, `memory.peak`, `memory.stat` file cache and `cpu.stat` usage. Fail closed if expected controls or counters are unavailable.

Sample nominally every 50 ms plus observer command overhead. Take explicit boundary snapshots for phase CPU deltas and filter samples by those observer timestamps. Sampled peaks can miss short transients. `memory.peak` is a cumulative kernel high-water mark from container creation, never a reset phase peak; report it alongside sampled phase current-memory peaks. Disk allocation is a phase-end observation, not temporary-disk high-water. Request quantiles use sorted lower empirical quantiles, `floor((n-1)*p)+1`; one media/download request per repetition gives identical within-repetition quantiles and is not a tail-latency population. Idle CPU precision and variability are reported without inventing a calibrated threshold.

Proc/cgroup files are separate observations, not an atomic snapshot. Validate every record before timestamp filtering. Only the explicitly awaited, sequential phase-boundary probes determine CPU deltas; background probes may overlap them and must not be ordered by their completion timestamp to enforce CPU monotonicity. File cache and current memory can move between reads, so their independently reported values have no per-record inequality assertion. On failure, print round/phase, numeric curl/HTTP status and target exit/OOM state without capability URLs. Attempt every cleanup action even if an earlier one fails, report cleanup failures separately, and retain the original failure exit code.

The [final evidence](../evidence/dist-resource-measurement.md) owns executed results, failures, raw output and limitations. Published-image/tag revalidation, native AMD64/Windows/macOS execution, cold cache, memory pressure beyond the explicit cap, disk-full/crash/power-loss, SDK and full-guide journeys remain outside this measurement slice. A different final release source or protected build input needs fresh equivalent evidence under the publication contract.

## Counter references

Official HTML fetched on 2026-10-24, with SHA-256 of the retrieved content:

| Contract | Content SHA-256 |
| --- | --- |
| [Linux cgroup v2 memory and CPU counters](https://docs.kernel.org/admin-guide/cgroup-v2.html) | `b478c1bae99899df476d6b4989cbb516f7ca93c5f009cdff64b802bbc24f3d4b` |
| [Linux proc process RSS and CPU counters](https://docs.kernel.org/filesystems/proc.html) | `f1c9cfc8d9779e8bb339e9b91761036caf71d3e5a6eb5dda5c1c34e98500b5db` |
| [Docker resource flags](https://docs.docker.com/engine/containers/run/) | `5ee1bb972fc1bcbbc2f5c0da0ee2d5979b1984549832a5e6bf01485cc301866f` |
