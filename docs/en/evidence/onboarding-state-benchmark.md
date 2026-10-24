# Shared onboarding contract and native baseline

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [shared onboarding state](../slices/onboarding-state-contract.md). The measured server source is `60a4f03ff3c856c5153f421b0fda2ad8ed7a8e3b`; its runtime sources, manifests/lockfile and compiler pin were unchanged throughout measurement. Contracts and measurement code are separate from runtime implementation. DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014 remain PARTIAL; UX-002 remains IMPLEMENTED.

## Reproduction and measurement boundary

Prepare outside the exclusive measurement window. A new disposable work directory holds separate catalogs for startup at 10,000/100,000 objects and five empty small-object catalogs. Creation uses current `Catalog::put_object`, at most eight concurrent preparations, ordinary checksums/fsync/WAL FULL and a read-only count assertion. It does not populate SQLite directly. The fixture preparation cost is excluded from samples.

The following historical reproduction uses server sources `60a4f03` and the original dedicated example/scripts from `10d8651f016def7e639f416cad48c76d463f7015` in an isolated checkout. That original runner has two arguments and guards only the server tree. The current administrative fixture helper requires the later storage interface, and its runner takes an explicit third SOURCE argument and also guards the example. Current candidate commands and provenance are recorded in [administrative verification](shared-admin-interface.md); the old baseline numbers and raw output remain unchanged.

```sh
cargo build --release --locked --offline --bin gcs-mimic --example onboarding-state-benchmark
cargo test --locked --offline --example onboarding-state-benchmark
bash scripts/test-onboarding-state-benchmark.sh
bench_work=$(mktemp -d)
bash scripts/measure-onboarding-state.sh prepare "$bench_work"
# Reserve the coordinator's exclusive resource window before the next command.
bash scripts/measure-onboarding-state.sh measure "$bench_work" > benchmark.txt
jq -Rs -f scripts/summarize-onboarding-state.jq benchmark.txt > baseline.json
```

Use the same source/runtime profile, release compiler/lockfile, payloads, filesystem, host and cache policy for comparisons. The harness refuses source/manifest/lock/toolchain differences from the baseline and records binary digests. It permits only explicit IPv4 loopback for HTTP clients. No Google/metadata-server/SDK contact is involved. Native macOS is the only implemented measurement platform; this is not a container result or a cross-product performance comparison.

The integration target later advanced to `96bcaa0` with other tracks' changes. That main revision was **not measured** here. Reproduce the baseline in a checkout of `60a4f03` containing this slice's dedicated example/scripts; the guard intentionally rejects a later runtime tree. A later candidate needs its own explicit source pin and equivalent five-run measurements. Do not attribute these numbers to the integration target merely because the evidence is merged there.

Each of five repetitions creates and overwrites 10,000 1 KiB objects, uploads and downloads exactly 1 GiB in separate processes/CPU/RSS intervals, and restarts the stopped 10,000/100,000-object fixtures. Response counts/statuses, every small-object checksum, all returned sizes, increasing/nonreused generations and exact downloaded bytes are asserted. Directional CPU deltas exclude the generator. RSS uses 50 ms process sampling; startup RSS is observed at readiness. Startup wall time includes 10 ms readiness polling and command-launch overhead. Disk is allocated space at phase end, not temporary-disk peak. Cgroup/page-cache memory, cold cache and complete peak RSS remain unmeasured.

Keep all raw repetitions. For each positive lower-is-better metric, the allowed fraction is `max(0.10, 2 * (max-min)/median)` and the upper bound is `median * (1+fraction)`. Throughput is gated using elapsed seconds for the fixed work. If any raw server CPU repetition is missing or at or below 0.1 second, both raw and normalized CPU remain uncalibrated because ps resolves only 0.01 second. Zero samples also stay pending. Every applicable metric has an entry; missing/nonnumeric values produce a reason and missing round numbers. Inapplicable metrics are listed separately (startup has no request-latency population or bytes/GiB denominator, and uses readiness RSS instead of phase sampling). A later candidate compares five-run medians with these bounds and the same zero-error correctness assertions. Broad noisy bounds are visible limitations, never a claim of product speed or a reason to drop outliers.

## Checks already executed

| Check | Result |
| --- | --- |
| Rust harness self-checks | PASS: framing, hashes, nearest-rank percentiles and rejection of reused overwrite generations |
| Shell harness self-check | PASS: a non-ready child holds a real catalog; timeout and TERM reclaim the child and allow reopening its lock; fake upload/download CPU intervals remain 1 and 2 seconds respectively; filesystem type parsing rejects a directory label |
| Small HTTP correctness smoke | PASS: three created and three overwritten objects, checksums, sizes and ordered generations; not used for baseline calibration |
| `just check` | PASS on the reviewed harness source: format, Clippy, 673 passing workspace tests across 36 suites, compatibility and offline shell/publication checks; [raw output](onboarding-state-checks.txt) |
| Dependency freshness | Rust 1.99.1 and all direct Rust dependencies CURRENT; checker exit 1 for the pre-existing distroless runtime digest update. No dependency change in this slice |

## Measured results and frozen thresholds

Executed on 2026-10-24 in the coordinator's exclusive window, with no other workers building/testing. Verification ID: `onboarding-state-native-baseline`. All six workloads completed five times, 30 samples total; the harness exited 0 with its final PASS and empty stderr. Every sample has zero errors. The immutable input is macOS 27.0.1 (26A434), Apple M4 Pro, 64 GiB RAM, 14 logical CPUs and APFS, without a CPU/memory container limit. Compiler: Rust/Cargo 1.99.1, edition 2024. Exact lock/toolchain/binary digests and environment output are in [raw stdout](onboarding-state-benchmark.txt). All five full records and per-metric gates are in [the machine-readable baseline](onboarding-state-baseline.json).

| Workload | Wall median (s) | Wall min–max (s) | Allowed regression | Median upper bound (s) | Server CPU median / upper (s) |
| --- | --- | --- | --- | --- | --- |
| small-publish | 126.530006 | 118.819705–130.851524 | 19.0181% | 150.593643 | 25.08 / 36.06 |
| small-overwrite | 182.831692 | 175.319717–188.539991 | 14.4617% | 209.272240 | 33.40 / 41.76 |
| upload-1gib | 2.416781 | 2.219825–2.597043 | 31.2166% | 3.171217 | 2.45 / 2.83 |
| download-1gib | 0.267560 | 0.250260–0.386444 | 101.7972% | 0.539930 | 0.48 / 0.76 |
| startup-10000 | 0.190218 | 0.126579–0.358742 | 244.1020% | 0.654544 | 0.08 / PENDING |
| startup-100000 | 0.969047 | 0.855896–2.151879 | 267.4758% | 3.561013 | 0.75 / 1.69 |

Values in the table are rounded for display; the JSON's full-precision thresholds are authoritative. Upload and download CPU-seconds/GiB equal their CPU-seconds here because each direction transfers exactly 1 GiB. Each transfer repetition has one request, so its percentile fields equal that request's time and are not a population latency estimate. Small-object p50/p95/p99 and their independent bounds remain in the JSON.

| Workload | RSS min / median / max (KiB) | Median upper bound (KiB) |
| --- | --- | --- |
| small-publish | 10800 / 10832 / 10880 | 11915.2 |
| small-overwrite | 10800 / 10832 / 10880 | 11915.2 |
| upload-1gib | 9920 / 10032 / 10976 | 12144 |
| download-1gib | 10624 / 10784 / 11472 | 12480 |
| startup-10000 | 10064 / 10128 / 10224 | 11140.8 |
| startup-100000 | 10064 / 10144 / 10272 | 11158.4 |

The first four RSS rows are sampled phase maxima; startup rows are readiness observations. Neither measures a complete lifetime peak. End-of-phase allocated disk observations and their gates are retained in the JSON; temporary-disk high-water remains unmeasured.

Startup/download variability makes their time bounds loose: all repetitions, including the first, are retained under the un-evicted warm/mixed cache policy and observed-readiness overhead. Those bounds detect only large regressions; use a quieter controlled profile before tightening them or making a product performance promise. No speed improvement is claimed. The 10,000-object startup CPU observations are 0.15, 0.09, 0.08, 0.07 and 0.08 seconds: four are below the 0.1-second calibration boundary, so that CPU gate remains **PENDING**, with no numeric upper bound. A more precise measurement is future work, not a passing calibration case.

Unimplemented synchronization, staging overlap and import resume are not measured by an API-only surrogate. Their cases, cgroup memory, startup CPU calibration, later migration/backup restoration and all other new runtime cases remain PENDING in the ledger. No SDK, Docker build, Linux/Windows execution, cold-cache or host-power-loss verification was performed for this contract slice. Both generated feature views were regenerated from the ledger; no status/fidelity change means their contents remain identical.
