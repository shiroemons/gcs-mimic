# Read optimization investigations

This is the starting point for the read-performance investigations recorded on
2026-11-07–08. Read it before repeating a rejected experiment. Decisions apply
to the recorded workload and environment, not to every future implementation.
No experimental reader or transport change is adopted by this documentation.
Feature status remains in `compatibility/features.json`.

For the investigation process and commands, see
[finding performance bottlenecks](../performance-investigation.md) and its
[Japanese companion](../../ja/performance-investigation.md).

## Findings and decisions

| Question | What was actually examined | Finding | Decision and reason to revisit |
| --- | --- | --- | --- |
| What already improved the read path? | Read pool/cache, cached positional reads, pull-driven bodies, frame pooling, TCP_NODELAY, worker defaults and musl allocation | Implemented behavior and its historical measurements are in [read path performance](read-path-performance.md) | Do not propose these as missing optimizations; investigate a new remaining cost |
| Why did reads past the read cache stop near 8,000 per second, whatever the connections? | CPU profile, `futex` counts and two candidate builds, 100 KiB at c=1/8/32/128 | SQLite's process-wide allocation-statistics mutex and a statement recompiled on every binding; [results](read-path-performance.md#reads-past-the-read-cache-and-ram-reads) | Adopted: statistics off at build time and a stable query plan on the read connections. RAM reads now enter the read cache |
| Does a fixed 256 KiB frame help 10 MiB at c=128? | gcs-mimic current/fixed candidate/fake-gcs-server, JSON/XML, c=1/128, three rounds | Every c=1 comparison slowed; XML c=128 never improved in paired RPS; JSON direction changed | Rejected; revisit only with new workload or host evidence, not the best isolated sample |
| Does shrinking frames above 16 active bodies help? | A gcs-mimic candidate with lifetime-counted direct HTTP bodies; small/large objects and threshold boundaries | Matched-cache 10 MiB/c=128 did not improve; 1 MiB JSON/c=128 lost 12.2% RPS and used 15.2% more CPU/GiB | Rejected; a different admission policy needs its own mechanism and evidence |
| Does keeping the final full 1 MiB frame help? | A gcs-mimic candidate retaining small selections and fractional tails | 10 MiB reads fell from 13 to 10; median JSON RPS did not improve; XML c=1 slowed in every pair | Rejected; fewer syscalls alone did not establish a useful improvement |
| Where is large-read CPU spent? | Exact-release CPU accounting and a separate symbol-enabled Linux CPU profile | Exact release was 97.50% system CPU; two copy functions accounted for 59.29% of the symbol build's on-CPU self samples | Copying is a measured cost; memory-bandwidth saturation, lock-wait absence and the cause of the fake-server gap remain unproven |
| Can kernel-assisted sending avoid copies? | A separate nginx sendfile on/off experiment, not a gcs-mimic implementation | At 10 MiB/c=128, median nginx server CPU/GiB fell 75.3%; c=1 p99 worsened in all three pairs, and 1 KiB controls worsened | Mechanism evidence only; no gcs-mimic improvement or safe integration is established |
| Can gcs-mimic use sendfile directly? | Source review of the current Bytes body and Hyper 1.12.0 output interfaces, plus nginx source | Rust can invoke the syscall, but the current HTTP output API passes memory slices, not file ranges | Integration remains unimplemented and unmeasured; preserve HTTP framing, generation, cancellation, TLS and cold-I/O behavior before adoption |
| Should the runtime become thread-per-core? | Design review only | No native Linux-host reproduction of the triggering single-download deficit | Deferred; changing Tokio worker count is a separate experiment, not thread-per-core |

## Measurement sets

| Evidence | Recorded coverage | How to interpret it |
| --- | --- | --- |
| [Fixed frame report](read-frame-concurrency.md) / [samples](read-frame-concurrency.txt) | 36 measured cells and 36 warmups, 100 objects of 10 MiB; three 10-second rounds | Shared-host variation is large; retain all samples |
| [CPU profile](read-frame-concurrency.md#cpu-profile-follow-up) / [counters](read-frame-concurrency-profile.txt) | Release accounting and separate sampled profiles, with binary/tool hashes | Profiled throughput is not the uninstrumented baseline; the symbol build's machine code differs |
| [Adaptive frame report](adaptive-read-frames.md) / [samples](adaptive-read-frames.txt) | 72 measured cells; 12,823,273 requests; mixed and finite cold/warm diagnostics are separate | One short sample per steady-state cell after matched preparation; fake access logging and mimic healthchecks retain defaults |
| [Tail-frame report](read-tail-copy.md) / [samples](read-tail-copy.tsv) | 72 GCS cells, 5,809,104 requests; 20 separate nginx cells, 2,194,448 requests | Three samples per 10 MiB condition; one per control condition; fake access logging suppressed and mimic healthchecks disabled |

Every valid comparison above reported zero request errors. Byte-hash, Range,
cancellation and Rust/SDK checks have their own scope in each report; they are
not inferred from RPS. A passing test suite does not establish a speedup.

All measurements used an Apple-silicon macOS host and Docker Desktop ARM64 Linux.
They are not native Linux-host results. Do not combine rows from different
reports into one before/after comparison: cache histories, logging, healthchecks,
sample durations and host conditions differ. fake-gcs-server is a comparison
implementation, not the API contract or a fixed performance target.

## Lessons that prevent misleading reruns

- Evicting only the adaptive candidate's cache produced an apparent c=1 gain
  where its framing policy did not change. That comparison was excluded for a
  documented preparation mismatch. All variants were prepared alike before the
  retained comparison. Warm up does not mean cache history can be ignored.
- The tail experiment's current JSON/c=128 RPS ranged from 1,438 to 2,965.
  Keep that slow sample. Its cause was not established, and a median is not a
  confidence interval or proof that a small difference comes from the patch.
- The historical Go client can exit zero while printing nonzero `errs`.
  Check both. It drains whole bodies; a header-only timing is a different metric.
- On-CPU self samples do not measure mutex sleep, disk wait or scheduler wait.
  Software sampling in this VM did not provide hardware cache-miss or bandwidth
  counters. Do not turn a copy hotspot into a memory-bandwidth diagnosis.
- Process RSS, cgroup memory and page-cache ownership are different quantities.
  nginx shared one file set across two cgroups, which charged its cache mostly
  to one side. Those memory figures cannot compare sendfile memory consumption.
- Keep other projects and self-hosted runners running unless their owner has
  explicitly authorized stopping them. The historical runs did not stop them.

## Candidates that have not been measured

| Candidate | Workload to test first | What could invalidate the idea |
| --- | --- | --- |
| Existing Tokio worker setting, for example 4/6/8 | 10 MiB at c=1 and c=128, plus CPU/GiB and p99 | Reducing workers may reduce throughput; adding workers may consume more shared CPU without useful gain |
| Bounded sharing of immutable-generation chunks | Concurrent requests overlapping the same generation and range | A cyclic 100 × 10 MiB working set may have few hits; budgets must include response-held bytes after eviction |
| Less metadata cloning and repeated stable-header formatting | Small objects and metadata GETs | It may not materially affect the large-body kernel copy cost |
| Narrower read-cache invalidation | Reads mixed with writes to different objects or buckets | Missing policy or metadata dependencies could return stale results; read-only throughput should not be credited with this change |
| Read-ahead hints | Controlled cold-file reads and interleaved ranges | Warm-cache reads do not need prefetch; advice on shared file descriptions can affect other reads |
| mmap or kernel-copy transport | A separate ownership/failure and transport investigation | Mapping alone does not prove safe external-file lifetime; sendfile does not remove page-cache lookup or handle current TLS automatically |

None is an implemented optimization or a promised percentage gain. Choose one
with a concrete workload and failure model before adding code. Retain checksum,
authorization, durability and bounded-resource behavior.

## Preservation boundaries

The reports, per-run text/TSV measurements, selected profile counters and
artifact hashes are retained in the repository. The original
[load client](read-load-client.go.txt) and
[nginx off configuration](nginx-sendfile-off.conf.txt) are archived verbatim as
measurement inputs; the on configuration changes only `sendfile off` to `on`.

The adaptive and tail tables preserve computed CPU-pressure percentages, not
the original pressure-total deltas and elapsed intervals. Those percentages
cannot be independently recomputed from the archive; CPU/GiB can.

Rejected source patches, executable images and large binary `perf.data` files
are not part of this archive. Machine-local worktrees and files mentioned in
historical reports are not durable repository inputs. Candidate hashes identify
what was measured, but descriptions alone are not exact rebuildable source
snapshots. Preserve the hypothesis and conclusion when source layouts change;
revisit a decision only when new evidence addresses its stated limitation.
