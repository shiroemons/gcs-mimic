# HTTP read frame sizing investigation

[Investigation index](read-optimization-summary.md) ·
[Measurement and diagnosis guide](../performance-investigation.md)

Measured on 2026-11-07, from commit `9a823d3028b65e0212e0799b6335be3b765ab062`.
This investigates the 10 MiB, 128-connection result in
[read path performance](read-path-performance.md). It does not add an API feature
or change the compatibility ledger. DESIGN-070 remains PARTIAL and DESIGN-079
remains IMPLEMENTED.

## Result and decision

Do not adopt a constant 256 KiB cap. Every single-connection comparison was
slower, and none of the XML 128-connection comparisons improved in RPS.
Adaptive sizing remains unproven: the JSON 128-connection direction changed
between runs, and the shared-host variation exceeds the original 6–12% gap.
Do not choose an adaptive threshold from these measurements. Thread-per-core
remains deferred pending the agreed native Linux-host reproduction.

The later [adaptive-frame candidate and verification](adaptive-read-frames.md)
records the per-server experiment, broad functional checks and a comparison with
matched page-cache preparation. That source candidate remains experimental.

### Throughput matrix

RPS, higher is better. Each cell is **median (minimum–maximum)** of three
ten-second runs. These are observations on a shared host, not isolated effect
estimates. All 36 measured cells and 36 warmups had zero reported errors and
zero process exit statuses. See the [complete raw output](read-frame-concurrency.txt).

| Request | Connections | fake-gcs-server 1.56.1 | Current mimic | 256 KiB candidate |
| --- | ---: | ---: | ---: | ---: |
| JSON media | 1 | 364 (352–408) | 359 (355–395) | 314 (300–319) |
| JSON media | 128 | 1,113 (277–2,392) | 1,269 (818–2,365) | 1,902 (1,470–2,288) |
| XML media | 1 | 434 (344–438) | 352 (303–397) | 280 (258–348) |
| XML media | 128 | 1,939 (399–2,207) | 2,263 (1,495–2,268) | 2,200 (1,445–2,217) |

The median alone makes JSON at 128 connections look about 50% faster after
shrinking frames. The individual comparisons show why that is insufficient:

| Request | Run | fake-gcs-server RPS | Current RPS | 256 KiB RPS | Candidate vs current |
| --- | ---: | ---: | ---: | ---: | ---: |
| JSON, c=128 | 1 | 1,113 | 1,269 | 1,902 | +49.9% |
| JSON, c=128 | 2 | 277 | 818 | 1,470 | +79.7% |
| JSON, c=128 | 3 | 2,392 | 2,365 | 2,288 | −3.3% |
| XML, c=128 | 1 | 399 | 1,495 | 1,445 | −3.3% |
| XML, c=128 | 2 | 2,207 | 2,268 | 2,200 | −3.0% |
| XML, c=128 | 3 | 1,939 | 2,263 | 2,217 | −2.0% |

The current build did not consistently reproduce the earlier deficit to
fake-gcs-server either. No run is selected as the uncontended result; even the
third run has XML reference p99 over 400 ms. Lower tail latency in some candidate
runs is worth investigating on an otherwise idle host, but is not proof that
cache working-set size caused the difference.

### Latency matrix

Each cell is p50 / p95 / p99 in milliseconds, rounded to three decimals, with
the median taken separately for each metric over the three runs. Lower is better.
These are not pooled-request percentiles.

| Request | Connections | fake-gcs-server 1.56.1 | Current mimic | 256 KiB candidate |
| --- | ---: | --- | --- | --- |
| JSON media | 1 | 2.666 / 3.631 / 5.161 | 2.626 / 3.446 / 4.151 | 3.100 / 4.173 / 5.035 |
| JSON media | 128 | 49.617 / 582.766 / 1528.589 | 56.686 / 199.366 / 1246.602 | 55.840 / 149.049 / 316.238 |
| XML media | 1 | 2.121 / 2.972 / 3.825 | 2.796 / 3.282 / 3.803 | 3.196 / 5.581 / 8.336 |
| XML media | 128 | 46.837 / 157.071 / 405.290 | 48.553 / 121.730 / 178.944 | 50.322 / 125.347 / 173.474 |

## Experiment

The baseline and candidate use the same repository Dockerfile, Rust 1.99.1,
Cargo.lock, musl, mimalloc, fat LTO and `opt-level="s"`. Both use eight Tokio
workers, eight blocking threads, disk-backed storage, permissive authentication,
HTTP/1.1 without TLS and default connection limits. The candidate caps HTTP
frames, including read-ahead and blocking fallback, at 256 KiB.
It leaves buffer pool retention, the 64 KiB `next_chunk` interface, authentication,
checksums, durable writes and runtime topology unchanged. A full 10 MiB download
uses 40 frames instead of nine 1 MiB frames and four 256 KiB frames, assuming full
reads. It is a fixed-size experiment, not an adaptive implementation.
The fixed-size source edit was removed after building the comparison images.
The subsequent adaptive experiment is described separately in
[adaptive HTTP read frames](adaptive-read-frames.md).

The reference is fake-gcs-server 1.56.1 with `-scheme http`,
`-filesystem-root /storage` and `-public-host gcs-frame-fake:4443`. The last flag
makes the XML URL use the same origin as the JSON URL. The three servers have
separate named volumes on one dedicated Docker network. No ports are published.

Each holds 100 objects of 10 MiB. The existing external Go closed-loop client
requests every object in turn using keep-alive, drains every body, and counts
transport failures, non-200 responses and incorrect body lengths as errors.
Latency includes receiving the last byte. RPS uses actual elapsed time including
draining requests started before the deadline. Each measured cell runs for ten
seconds after a separate three-second warmup. Each cell runs three times;
server order rotates through fake/base/small, base/small/fake, small/fake/base.
This is a short local comparison, not a regression gate or a capacity promise.

The [client source](read-load-client.go.txt) is archived verbatim. The client
was reused without modification; it was built with Go 1.24.5,
`CGO_ENABLED=0`, `GOOS=linux`, `GOARCH=arm64`. It reports error counts without a
nonzero exit status, so both the counts and process exits must be checked.

The external client command for each cell is below. `CLIENT_DIR` contains the
identified binary; `VARIANT` is `fake`, `base` or `small`, `KIND` is `media` (JSON)
or `public` (XML), and `C` is `1` or `128`. Run with `-d 3s` for warmup, then
`-d 10s` for measurement. The raw output columns are request kind, concurrency,
RPS, p50/p95/p99/max in microseconds, and error count.

```sh
docker run --rm --network gcs-frame-investigation \
  --mount "type=bind,src=$CLIENT_DIR,dst=/loadgen,readonly" \
  alpine@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a \
  /loadgen/loadgen-linux -base "http://gcs-frame-$VARIANT:4443" \
  -bucket load -objects 100 -size 10485760 -kind "$KIND" -c "$C" -d 10s -csv
```

| Artifact | SHA-256 |
| --- | --- |
| Baseline image index | `a17546d6cd070a676f6b891df157fdafc035d4a3f0b1fc52a54d4ad93a11bdf1` |
| 256 KiB image index | `e86188464b350dddbdd94754cd860f96ac4c81c67269e7bf9040ed9b310b35d9` |
| fake-gcs-server image | `797ce226d62f947c009dc40246b30cfb456b8473d8241407f9d6f2c04e4d69ef` |
| Client container (Alpine 3.24) | `be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a` |
| External client source (`main.go`) | `9c36385be49534451fdf62c3d71bbc39dbc71e38efe5370ca0cbc0e79cabbf73` |
| External client binary | `6cb22ae3fd4f8301d4d05fd52c1fac36fc09023cbb4d99c447445138a3fdf462` |

## Environment and interpretation limits

The host is Apple silicon macOS. Docker Desktop exposes 14 vCPUs and 15.6 GiB
to its linux/arm64 VM, with kernel `7.0.14-linuxkit` and transparent huge pages
set to `always`; both mimic binaries disable huge pages as usual. No explicit
container CPU quota, CPU affinity or memory cap is added.

All builds and Rust tests finish before measurement. Other project test load
subsided before the first attempt, but multiple host VM processes were still
busy. The user asked to keep GitHub Actions/self-hosted runners running; none
were stopped. This is a shared host, so order rotation does not remove all
interference, and short-run differences cannot establish an isolated CPU-cache
or scheduler effect. A preliminary attempt with an incorrect reference XML host
returned errors and is excluded from the final comparison.

The frame-size comparison above did not measure process RSS peak, cgroup peak,
CPU-seconds/GiB, hardware cache misses, CPU migrations or clock frequencies.
The follow-up profile below adds CPU accounting and sampling. The theoretical in-flight buffer
reduction from 128 MiB to 32 MiB is not a measured RSS saving. Pool retention and
allocator retention are separate from buffers in flight.

## CPU profile follow-up

The hot path is copying file data through userspace into a TCP socket. On the
unmodified release binary, 97.50% of accounted CPU time was system time. A
separate symbol-enabled profile attributed 59.29% of on-CPU samples to the two
copy functions below. This supports investigating copy avoidance before
optimizing progress notifications or buffer-pool locks; it does not establish
memory-bandwidth saturation, absence of lock waiting, or a sendfile speedup.

### CPU cost and contention

One ten-second JSON media run per implementation, c=128, 100 objects of 10 MiB,
after a three-second warmup. These runs used `perf stat`, not stack sampling.
The exact existing release images were reused. All requests completed without
reported errors; both process exits and `errs=0` were checked. Each run was
sequential on the shared Docker Desktop host, with existing runners left running.

| Metric | Current release | Fixed 256 KiB | fake-gcs-server 1.56.1 |
| --- | ---: | ---: | ---: |
| Completed requests | 23,494 | 25,530 | 16,123 |
| RPS | 2,339 | 2,543 | 1,503 |
| Received GiB | 229.434 | 249.316 | 157.451 |
| Server CPU seconds | 77.579 | 72.967 | 74.568 |
| CPU seconds/GiB, lower is better | 0.3381 | 0.2927 | 0.4736 |
| System share of accounted CPU | 97.50% | 96.64% | 90.68% |
| Context switches | 14,677 | 30,599 | 123,016 |
| Context switches/GiB | 64.0 | 122.7 | 781.3 |
| CPU migrations | 286 | 190 | 4,102 |
| Page faults, minor and major combined | 95,444 | 4,534 | 9,558 |
| CPU PSI some, interval estimate | 2.15% | 6.13% | 50.29% |
| CPU PSI full, interval estimate | 2.13% | 6.00% | 22.59% |
| Cgroup CPU throttling events | 0 | 0 | 0 |

CPU seconds come from the server cgroup's `cpu.stat` usage delta, excluding the
client and observer. Divide by `completed requests * 10 / 1024` for CPU/GiB.
The independent `perf` task-clock totals were 77.497, 72.881 and 74.509 seconds.
Snapshots bracket the profiler command with a small setup/teardown interval.

CPU PSI percentages divide the cgroup pressure `total` delta by the profiler's
elapsed time; they are interval estimates, not the kernel's moving `avg10`.
`some` means at least one task was waiting for CPU; `full` means all non-idle
tasks in the cgroup were waiting. They measure wall-time pressure, not a sum of
thread wait times, CPU utilization, or delay caused specifically by other
projects. See the [kernel PSI documentation](https://docs.kernel.org/accounting/psi.html).

The fixed cap used 13.44% fewer CPU seconds/GiB in this observation, while
context switches/GiB rose by 92%. This keeps adaptive sizing worth investigating,
but does not override the earlier single-connection regressions or establish a
repeatable improvement. The fake server's next, sampled run reached 2,414 RPS
and 0.3076 CPU seconds/GiB. Its 1,503 RPS result is not a stable performance
baseline. No CPU quota or affinity was imposed, and runtime configurations were
not normalized: mimic used eight Tokio workers; Go used its image defaults.

### Where CPU samples landed

Separate ten-second runs used software `cpu-clock` sampling at 199 Hz with
8,192-byte DWARF stack capture. Percentages are **self samples across the whole
sampled server process**, not inclusive caller percentages or request latency.
The mimic baseline was rebuilt with line tables and symbols; the other two
used their existing stripped release images, so their userspace function names
are unresolved. Kernel symbols and copy call chains resolved for all three.

| Sampled function or group | Current, with symbols | Fixed 256 KiB | fake-gcs-server |
| --- | ---: | ---: | ---: |
| `__arch_copy_to_user` | 39.55% | 39.24% | 16.26% |
| `__arch_copy_from_user` | 19.74% | 18.86% | 18.19% |
| Two copy functions combined | 59.29% | 58.10% | 34.45% |
| `filemap_get_read_batch` | 7.33% | 6.05% | 0.15% |
| All kernel samples | 97.78% | 96.83% | 90.46% |
| Samples recorded | 13,408 | 10,302 | 13,494 |
| Lost samples reported | 0 | 0 | 0 |
| RPS during sampling | 1,872 | 1,863 | 2,414 |
| CPU seconds/GiB during sampling | 0.4019 | 0.2905 | 0.3076 |

The resolved mimic call chains were:

```text
ObjectBody::poll_frame / ObjectReader::read_ahead_cached
  -> read_cached_chunk -> preadv2 -> filemap_read
  -> copy_page_to_iter -> __arch_copy_to_user

Hyper poll_write -> TcpStream::poll_write_vectored -> writev
  -> tcp_sendmsg_locked -> __arch_copy_from_user
```

The fake server also copied through userspace: `read -> filemap_read ->
copy_page_to_iter -> __arch_copy_to_user`, and `write -> tcp_sendmsg_locked ->
__arch_copy_from_user`. Copying is shared by both implementations, so identifying
it does not by itself explain their RPS difference. The different percentages
have different denominators and execution conditions; they do not measure
bytes copied, memory bandwidth or cache misses. The large difference in
`filemap_get_read_batch` is an observation, not an established mechanism.

In the symbol-enabled mimic sample, `Notify::notify_waiters` had 0.06% self
samples and `ReadBuffer::take` 0.01%. Inlining and off-CPU waits prevent treating
these as the total costs of notification or locking. Nonetheless, this profile
provides no evidence to prioritize either over the copy path.

### Reproduction and limits

The [profile evidence](read-frame-concurrency-profile.txt) records client output,
accounting inputs, profiler totals, selected self samples and artifact hashes.
Large binary profiles and the symbol-enabled executable are retained outside
the repository. The repository's production sources remain unchanged.

The observer container shared only the server's PID namespace, with a separate
cgroup for observer and client processes. It used `PERFMON`, `SYS_PTRACE` and
`SYSLOG` capabilities and an unconfined seccomp profile, without changing host
sysctls. `perf` 7.2.12-r0, binutils 2.45.1-r1 and sysstat 12.7.8-r0 were installed
only in the dedicated profiling image. Hardware `cycles`/`instructions` events
were unsupported in this VM, so no hardware cache or bandwidth claims are made.

With `LOADGEN` pointing to the previously identified Linux client, the core
commands inside the observer were:

```sh
perf stat -p 1 -e task-clock,context-switches,cpu-migrations,page-faults \
  -o /profile/counters.txt -- \
  "$LOADGEN" -base "$BASE" -bucket load -objects 100 -size 10485760 \
  -kind media -c 128 -d 10s

perf record -e cpu-clock -F 199 --call-graph dwarf,8192 -p 1 \
  -o /profile/perf.data -- \
  "$LOADGEN" -base "$BASE" -bucket load -objects 100 -size 10485760 \
  -kind media -c 128 -d 10s

perf report -i /profile/perf.data --stdio --no-children -g none \
  --sort dso,symbol --percent-limit 0.5
```

The symbol build kept Rust 1.99.1, musl, fat LTO and `opt-level="s"`, changing
only the release debug/strip configuration to `debug="line-tables-only"` and
`strip="none"`. Its `.text` hash differs from the original release, so the
profile is not claimed to be instruction-identical. Build effects, sampling
overhead and changing host contention are not separately quantified.

All three recordings reported `Failed to open /proc/schedstat`; the symbol
recording also reported one out-of-order event. Recording and reporting exited
successfully, with zero lost samples reported. Per-thread `schedstat` remained
readable. Matching thread IDs gave 6.597 summed runnable-wait seconds for the
symbol-enabled baseline (20 threads throughout), 26.853 for the sampled fixed
cap (20 throughout), and 154.785 for the unsampled fake run (45 to 48 threads,
three additions, no observed exits). These sums are not elapsed seconds or
mutex/I/O wait times. Exited threads made exact-release and fixed-cap counter-run
totals incomplete, so those totals are not used for comparison. Thread snapshots
also cannot capture a thread born and exited entirely between snapshots.
See the [scheduler statistics documentation](https://docs.kernel.org/scheduler/sched-stats.html).

The next focused optimization experiment would examine bounded copy avoidance
for untransformed plaintext file responses, while retaining the normal path for
other responses. Linux [sendfile](https://man7.org/linux/man-pages/man2/sendfile.2.html)
avoids the userspace round trip, but integrating it with the Hyper transport,
backpressure, immutable generation selection, cold-file behavior and cancellation
requires a separate design and measurement. No sendfile implementation, adaptive
policy, thread-per-core conversion, TLS profile, XML profile or native Linux-host
profile was performed in this follow-up.

## Adaptive sizing and runtime design

The existing pools each use a global mutex for take and return, and each frame
records transfer progress. Smaller frames reduce buffer size but increase these
operations and socket-write opportunities. The static comparison must support
the premise before adding an adaptive counter or choosing a threshold.

If adaptive sizing is justified, count the lifetime of direct HTTP object bodies,
not accepted TCP connections: idle keep-alive, TLS handshakes and gRPC connections
are different loads. An HTTP-layer RAII guard can own the count, with the chosen
capacity passed to both `next_frame_at_hand` and `next_frame`. This keeps storage
independent of the server and avoids adding `Drop` to `ObjectReader`, whose
consuming methods move out its fields. Count changes and reads would estimate
load, not synchronize object state.

The design must cover unpolled drop, cancellation during a blocking read, read
errors, normal completion and Hyper ending via `is_end_stream` without another
poll. Read-ahead already exists before `ObjectBody` is constructed. Keeping its
first 1 MiB frame is a limitation of the smallest adaptive prototype; splitting
`Bytes` does not release that original allocation. Avoid claiming a strict
256 KiB per-body bound unless read-ahead admission is changed as well.

Thread-per-core remains deferred until a native Linux host reproduces the
single-download difference. One multi-thread runtime with one worker is not a
thread-per-core implementation. Multiple current-thread runtimes require
connection distribution, shared admission/reclamation, supervised task lifetime
and shutdown/drain design. In particular, `task::run_blocking` uses the current
runtime: duplicating the existing eight-thread blocking pool per runtime would
multiply the process-wide limit. See the
[Tokio 1.54.1 runtime documentation](https://docs.rs/tokio/1.54.1/tokio/runtime/).

## Verification

- `cargo check --locked --workspace`: passed on the candidate.
- `cargo test --locked --lib server::object_body::tests`: 7 passed, 0 failed.
- `cargo test --locked --lib storage::catalog::objects::reader::tests`: 3 passed,
  0 failed.
- Both linux/arm64 Docker builds completed with the repository Dockerfile.
- Native Linux-host comparison, runtime restructuring, full SDK suites and
  full workspace tests were not run for this investigation.
