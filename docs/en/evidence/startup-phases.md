# Startup phase verification

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [startup phases](../slices/startup-phases.md), covering the startup subset of DESIGN-062, DESIGN-072 and DESIGN-074. All three broad feature rows remain PARTIAL. UX-002 remains IMPLEMENTED.

## Runtime verification

Runtime source: `b2b8b094c43c05d5c151c706ae2df747ec70a112`. Phase implementations live in `src/startup.rs`; `src/seed.rs` changes only its result representation to SeedOutcome. Shared diagnostics now combine the primary and catalog-close results before and after readiness. No recovery, schema, blob publication, configuration, URL, authentication or TLS implementation change belongs to this slice; the storage/policies change adds only a cancellation test.

| Command | Result |
| --- | --- |
| Targeted binary/foundation/seed tests | PASS: 7 binary, 19 foundation and 13 seed tests, including recovery/PAP barriers, exact ready keys, close error composition and real SIGTERM during partial staging/after rename. |
| `just check` | PASS on the final runtime source: 701 tests across 36 suites, format, Clippy, compatibility and offline shell/publication checks; [output](startup-phases-checks.txt). |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS. |
| `cargo xtask dependencies check` | Exit 1: Rust 1.99.1 and all direct Rust dependencies CURRENT; pre-existing distroless runtime digest UPDATE_AVAILABLE. No dependency change. |

The [contract's acceptance table](../slices/startup-phases.md#acceptance-evidence) maps every startup behavior to its named tests. Native tests run outside the sandbox because sandboxed localhost binding is denied. Configuration/TLS/registry preflight remains synchronous before signal registration. Recovery is drained on interruption, without a fixed completion deadline. New administrative/import/publication contracts remain PENDING.

## Startup measurement procedure

Build and prepare outside the exclusive resource window:

```sh
cargo build --release --locked --offline --bin gcs-mimic --example onboarding-state-benchmark
bench_work=$(mktemp -d)
target/release/examples/onboarding-state-benchmark prepare "$bench_work/startup-10000" 10000
target/release/examples/onboarding-state-benchmark prepare "$bench_work/startup-100000" 100000
```

The existing fixture generator uses `Catalog::put_object`, 1,024-byte bodies, eight bounded producers, two async/two blocking workers, unchanged checksums/fsync/WAL FULL, clean close and read-only exact-count assertions. Preparation is excluded from the measurements. Each stopped fixture is reused for five startup repetitions, matching the [baseline profile](onboarding-state-benchmark.md#reproduction-and-measurement-boundary).

Executed fixture preparation uses the existing generator built from runtime source `31a1275`, SHA-256 `2726f1027750036a717cab623bd5c6659d7aa415473075f270a3978afa73112a`. The subsequent phase/type changes alter neither Catalog::put_object nor the fixture schema, payloads or seed-completion effects. The candidate server is rebuilt from the final runtime source above; these are not measurements of the earlier server.

Reserve and receive the coordinator's exclusive-window approval before measurement. Use the existing startup/readiness/shutdown helpers without changing their polling or timing boundaries:

```sh
source scripts/measure-onboarding-state.sh
work=$bench_work
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
for round in 1 2 3 4 5; do
  for count in 10000 100000; do
    start_server "$work/startup-$count"
    seconds=$(awk -v before="$started" -v after="$finished" \
      'BEGIN {printf "%.6f",after-before}')
    startup_cpu=$(cpu)
    rss=$(ps -p "$pid" -o rss= | tr -d ' ')
    disk=$(du -sk "$work/startup-$count" | awk '{print $1}')
    jq -cn --argjson round "$round" --argjson count "$count" \
      --argjson seconds "$seconds" --argjson cpu "$startup_cpu" \
      --argjson rss "$rss" --argjson disk "$disk" \
      '{round:$round,workload:("startup-"+($count|tostring)),objects:$count,
        wall_seconds:$seconds,server_cpu_seconds:$cpu,ready_RSS_KiB:$rss,
        allocated_disk_KiB:$disk,errors:0,cgroup_memory:null}'
    stop_server
  done
done
```

Record the source/lock/toolchain/binary digests, host and filesystem before the loop, along with simultaneous-load and cache conditions. Compare all four applicable startup metrics against `onboarding-state-baseline.json`; keep every sample and the original frozen upper bounds. A PENDING baseline CPU gate stays PENDING. Startup has no latency population or transfer-throughput denominator, and readiness RSS is not peak RSS. Cgroups, cold cache, Windows/Linux execution, SDK/container journeys and host power loss remain unmeasured. Delete the disposable fixtures after verification.

The baseline lock digest is `3b8100d00d9d1b534fce001b07e8329cd2d26aaba19bdfe4678dd79ae718116b`; the candidate's is `58e2794c0a27d78f509123d5c39182ffe654963ab54363cf7e6dc0587916c63b`. Before this slice, approved direct serde/derive and proptest dev-dependency additions changed the lock graph, without upgrading existing library versions. The compiler pin and runtime settings remain equivalent. This comparison evaluates the pinned candidate build against the recorded baseline; it does not isolate the phase refactor from intervening source/build-graph changes or establish a speed improvement.

## Measurement result

Executed on 2026-10-24 after source review and the final 701-test check, in the coordinator-approved exclusive window. The coordinator confirmed all three other workers were stopped before measurement; no worker builds/tests overlapped. Host: macOS 27.0.1 (26A434), Apple M4 Pro, 64 GiB RAM, 14 logical CPUs, APFS, without container CPU/memory limits. HTTP/1 IPv4 loopback, permissive/native profiles, two async/two blocking workers, SQLite WAL/FULL, disk blobs and un-evicted sequential warm/mixed caches match the baseline profile. The shell uses Bash, as required by the existing harness.

The startup-only loop completed all ten records with exit 0, empty stderr, errors=0, successful ready probes and graceful stopped events. Fixture generation asserted the exact live-object counts through a read-only catalog query after ordinary publication/close. [Raw stdout](startup-phases-benchmark.txt) includes compiler, toolchain, source/lock/binary digests and every repetition. [Machine-readable comparisons](startup-phases.json) retain all samples, variability and frozen baseline bounds. No failed run, missing record or outlier is silently excluded.

| Workload | Wall min / median / max (s) | Baseline wall median / upper bound (s) | CPU median / upper bound (s) |
| --- | --- | --- | --- |
| startup-10000 | 0.087599 / 0.091747 / 0.515867 | 0.190218 / 0.654544 | 0.07 / PENDING |
| startup-100000 | 0.727584 / 0.777482 / 1.737045 | 0.969047 / 3.561013 | 0.69 / 1.69 |

| Workload | Readiness RSS median / bound (KiB) | Allocated disk median / bound (KiB) |
| --- | --- | --- |
| startup-10000 | 10352 / 11140.8 | 44036 / 48598 |
| startup-100000 | 10432 / 11158.4 | 432864 / 476137.2 |

PASS: candidate medians meet all seven calibrated startup gates. startup-10000 CPU samples are 0.09, 0.07, 0.07, 0.07 and 0.07 seconds; the existing precision-limited CPU gate remains PENDING with no numeric bound. The higher first wall-time samples remain included. These warm/mixed-cache observations and broad historical bounds do not establish a speed improvement or a shutdown deadline; in particular the observed startup-100000 range is not a bound on interrupted recovery. Source/build-graph differences are disclosed above. RSS is observed after readiness, not peak RSS, disk is phase-end allocation, and cgroup memory is unmeasured. There is no request-latency/transfer-throughput population for this startup-only workload.

The fixed-baseline comparison groups the raw JSON records by workload, requires rounds 1–5 exactly once, zero errors and the exact object count, sorts each metric's five values and compares the middle value to the existing baseline's median_upper_bound. It never recalibrates the old bounds. A baseline gate without a numeric bound stays PENDING. The comparison was self-checked with the baseline's own samples, reproducing its medians and sole pending CPU metric before processing the candidate. Check the stored gate result with:

```sh
jq -e '.result == "PASS" and .pending_metrics == ["startup-10000/server_cpu_seconds"]' \
  docs/en/evidence/startup-phases.json
```

The exclusive window was released immediately after the loop. Disposable fixture catalogs were removed after verifying the recorded results. No SDK/container journey, cgroup, cold-cache, Windows/Linux execution or power-loss test was performed for this slice.
