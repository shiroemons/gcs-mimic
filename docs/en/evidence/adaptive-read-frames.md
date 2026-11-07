# Adaptive HTTP read frames: verification and measurements

[Investigation index](read-optimization-summary.md) ·
[Measurement and diagnosis guide](../performance-investigation.md)

Measured and verified on 2026-11-07–08. This is an experimental follow-up to the
[fixed-frame investigation](read-frame-concurrency.md),
based on commit `9a823d3028b65e0212e0799b6335be3b765ab062`. It changes no API feature
status: DESIGN-070 remains PARTIAL and DESIGN-079 remains IMPLEMENTED.

## Decision

Do not adopt this candidate. Functional verification passed, but the comparison
with matched cache preparation did not establish the intended 10 MiB/c=128 throughput or CPU benefit,
and smaller-object cases showed worse throughput and CPU cost. Retain the
candidate and its tests as investigation material in the separate worktree;
no candidate changes have been merged into the baseline branch. Passing correctness checks is not evidence
of a performance improvement or a universal no-regression guarantee.

## Candidate and safety boundaries

The candidate counts live direct-transfer JSON/XML `ObjectBody` responses per
server; paths such as on-the-fly decompression are not counted. Above 16 live
bodies, newly selected reads use at most 256 KiB; otherwise they retain the
existing frame size, at most 1 MiB. This is not a connection count. The threshold
is experimental, not a measured hardware-cache capacity. Small responses also
contribute to the count. A read already in progress keeps its previously selected
limit until it completes.

An RAII guard releases the count when a body is dropped, including cancellation
and failure. A relaxed atomic estimates load; it does not synchronize object
contents. A two-variant `FrameLimit` prevents zero or unbounded read sizes. The
reader still owns the selected immutable generation, uses positioned reads,
preserves errors, and sends only the bytes actually read. No new dependency or
unsafe block is introduced. gRPC's 64 KiB chunk interface is unchanged.

The initial read-ahead can still allocate 1 MiB before a body is counted.
Splitting that `Bytes` retains its original allocation until the final slice is
dropped. Therefore 256 KiB is a frame limit, not a strict allocation or RSS bound.
The candidate does not remove either file-to-userspace or userspace-to-socket
copies, and it does not bypass page-cache lookup. Authentication, checksums,
durability and runtime topology are unchanged.

## Verification matrix

The following checks use the final candidate source. Counts exclude repeated
focused runs; they do not count SDK assertions as Rust tests.

| Area | Coverage | Result |
| --- | --- | --- |
| Rust compilation | `cargo check --locked --workspace --tests` | Passed |
| Rust library | Native macOS unit, property, concurrency, storage and recovery tests | 1,752 passed |
| Binary tests | CLI/configuration and process helpers | 31 passed |
| Integration suites | Admin, seed/import, JSON, objects, process, protocol, sync, XML/auth | 938 passed; 1 existing opt-in Pub/Sub test ignored |
| Code generation | gRPC code-generation test | 1 passed |
| xtask | Release, lint, compatibility and supporting tools | 209 passed after correcting the toolchain environment |
| Documentation tests | Workspace doctests | 4 passed |
| Rust total | Library, binary, integration, codegen, xtask and doctests | **2,935 passed; 1 existing opt-in ignored** |
| Clippy | Workspace, all targets, `-D warnings` | Passed |
| Production lint | `cargo xtask lint` | 0 new violations; 0 ratcheted findings |
| Cross-target checks | Windows MSVC, Linux x86-64 glibc and musl | Type checking and production lint passed; not runtime tests |
| Linux release | aarch64 musl, Rust 1.99.1, fat LTO, size optimization, mimalloc | Checked and built |
| Official SDKs over HTTP | Go, Ruby, Node.js, Python, PHP, Java, .NET and Rust | All eight suites passed |
| Official SDKs over HTTPS | Go, Ruby, Node.js, Python and PHP | All five supported suites passed |
| Full-object bytes | Linux, 10 MiB, 128 concurrent GETs per disk/memory × JSON/XML cell | All 512 SHA-256 hashes matched |
| Range bytes | Linux, 32 concurrent GETs per disk/memory × JSON/XML cell; bytes 123–2097281 | All 128 SHA-256 hashes matched |
| Slow clients and cancellation | 17 clients limited to 1 KiB/s, each cancelled by a six-second timeout | All 17 cancelled; frames returned to normal; readiness stayed healthy |
| Cold read | Candidate blob cache evicted with `vmtouch`, then a full-object GET | Hash matched; `RWF_NOWAIT` returned EAGAIN and blocking fallback was observed |

The integration count is 72 + 77 + 135 + 144 + 76 + 146 + 30 + 258 = 938.
The aggregate includes a successful xtask rerun, not one completely successful
`cargo test --workspace` invocation. Its initial three failures were license-file
lookups: Homebrew Rust 1.99.1 lacks the official sysroot's license documents.
The same xtask test binary passed all 209 tests with official rustup Rust 1.99.1
first in PATH and local test sockets permitted. No test or assertion was removed.

The initial failure and successful rerun reported:

```text
called `Result::unwrap()` on an `Err` value: "No such file or directory (os error 2)"
test result: FAILED. 206 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.03s
test result: ok. 209 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.10s
```

SDK setup initially selected macOS system Bundler and, in a later HTTPS command,
uv 0.12.1. The final successful commands explicitly selected the installed Ruby
4.0.9/Bundler 4.0.24 and uv 0.13.7, without changing global installations.
The HTTP-only Java/.NET/Rust harnesses were not represented as HTTPS coverage.

### Direct Linux byte and cancellation checks

The random 10 MiB fixture has SHA-256
`df8eef8e83d6ce30f3b894f04a95c34ea5c00b0b30ad6d4443cd7bdb0229d849`.
Full GETs and unaligned ranges were compared to that fixture or its exact slice.
The load generator's separate length/status checks are not substitutes for these
content comparisons.

During the slow-client experiment, raw `preadv2` tracing of another full GET
observed one 1 MiB initial read and 36 reads of 256 KiB. After all 17 clients
disconnected, another full GET observed nine reads of 1 MiB and four of 256 KiB,
the normal framing of a 10 MiB object. Both GET hashes matched. Tracing was
restricted to syscall metadata and was not active during throughput runs.

The cold check evicted only this experiment's blob directory, not the host's
global cache. `vmtouch` reported 0 of 284,260 pages resident before the GET.
Tracing recorded two EAGAIN results and a `pread64` fallback; the response hash
matched. This verifies fallback correctness, not performance on a physical disk:
Docker Desktop's backing layers can remain cached.

## Reproduction

Use the repository's pinned toolchain and locked dependencies. These checks
require the installed SDK runtimes listed in `docs/en/development.md` and local
loopback networking. The Rust commands run before the SDK or performance runs.

```sh
cargo check --locked --workspace --tests
cargo test --locked --workspace
cargo test --locked --workspace --doc
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo xtask lint
bash scripts/check-cross.sh
bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php \
  --with-java --with-dotnet --with-rust
GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh \
  --with-ruby --with-node --with-python --with-php
```

The candidate executable SHA-256 is
`9f32fdf5385d45c5751dec8a8e9e56f36711b0757a14b25908f492c144885dbb`.
The baseline, fake-gcs-server and load-generator identities are recorded in the
[original investigation](read-frame-concurrency.md#experiment).

## Measurement controls

The measurements use the same shared Docker Desktop ARM64 host and external
closed-loop client as the original investigation. The three servers have
separate persistent volumes. Builds and SDK tests finish before timing starts;
unrelated runners are left running. Each size has 100 objects. A cell warms for
two seconds and measures for five seconds, including completion of requests
already started. Server order rotates between cells. Both client exit statuses
and the reported error counts are checked.

An initial exploratory comparison was excluded because only the candidate had
undergone the cold-read test. It showed 359 versus 509 RPS at c=1, where the
candidate does not shrink frames. This was a warning that the comparison did
not isolate adaptive sizing. All three experimental servers were restarted,
their own blob caches evicted, all three sizes scanned through the JSON API at
c=1, and 10 MiB objects primed at c=128. Each size's priming completed more than
100 requests without errors. The final comparisons use this common history.

Before priming, resident-page counts were 0/281,700 for baseline blobs,
0/284,260 for candidate blobs and 0/281,703 for the fake server's storage.
After priming they were 281,700/281,700, 281,700/284,260 and 281,700/281,703.
The candidate's extra 2,560 nonresident pages correspond to the separate 10 MiB
correctness fixture, which is not part of the timed dataset; fake retained three
nonresident pages. A single cold-state operation is not proof about folio sizes
or physical-disk behavior; these were not measured.

Logging also retains each implementation's defaults: fake-gcs-server emits an
INFO access-log entry per request, while mimic does not. The fake column compares
those default runnable profiles, not implementations normalized to identical
logging overhead. In particular, the small-object gap must not be attributed
solely to Rust or to the read-buffer algorithm.

CPU seconds are server-cgroup `usage_usec` deltas, excluding the load generator
and observer. CPU/GiB divides by completed, length-checked payload bytes. CPU
pressure is the interval delta of `cpu.pressure` totals divided by elapsed
`/proc/uptime` time, with approximately 10 ms clock resolution. It is not a
measure of mutex contention. No container CPU quota, affinity or memory cap is
set. Mimic uses its default eight async/eight blocking workers and fake uses its
image defaults. Mimic's image health checks are included in its cgroup accounting.

RSS is the server process's **end-of-cell snapshot**, not a peak. Cgroup memory
includes page cache and is reported separately. The allocator and buffer pools
retain memory between cells, so a snapshot must not be interpreted as a fixed
per-request memory cost or a proven peak-memory reduction.

## Throughput and CPU matrix

One short observation per cell after the matched preparation above. Higher RPS
and lower CPU seconds/GiB are better. Changes compare candidate with
current. These are observations on a shared host, not isolated effect estimates.
All 72 measured cells and 72 warmups exited successfully with zero reported
errors. The measured cells completed **12,823,273 requests**. Full latency,
CPU-pressure and memory values are in the [measurement table](adaptive-read-frames.txt).
CPU pressure is a saved derived value: the table does not retain its original
pressure-total deltas or elapsed intervals, so it cannot be independently
recomputed from this archive. CPU/GiB can be recomputed from the retained inputs.

| Size | API | Connections | Current RPS | Candidate RPS | fake RPS | RPS change | CPU/GiB change |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 10 MiB | JSON | 1 | 581 | 602 | 475 | +3.6% | −3.8% |
| 10 MiB | JSON | 16 | 2,583 | 2,749 | 2,333 | +6.4% | −5.8% |
| 10 MiB | JSON | 17 | 2,356 | 2,817 | 2,511 | +19.6% | −16.5% |
| 10 MiB | JSON | 128 | 2,894 | 2,855 | 2,681 | −1.3% | +0.0% |
| 10 MiB | XML | 1 | 541 | 608 | 436 | +12.4% | −10.8% |
| 10 MiB | XML | 16 | 2,317 | 2,762 | 2,322 | +19.2% | −13.9% |
| 10 MiB | XML | 17 | 2,787 | 2,592 | 2,401 | −7.0% | +6.5% |
| 10 MiB | XML | 128 | 2,720 | 2,661 | 2,457 | −2.2% | +1.7% |
| 1 MiB | JSON | 1 | 2,979 | 2,817 | 2,750 | −5.4% | +6.2% |
| 1 MiB | JSON | 16 | 21,983 | 19,422 | 17,990 | −11.6% | +13.4% |
| 1 MiB | JSON | 17 | 19,522 | 21,329 | 18,347 | +9.3% | −2.1% |
| 1 MiB | JSON | 128 | 23,682 | 20,787 | 20,259 | −12.2% | +15.2% |
| 1 MiB | XML | 1 | 3,034 | 2,759 | 2,913 | −9.1% | +10.6% |
| 1 MiB | XML | 16 | 22,755 | 20,655 | 18,737 | −9.2% | +10.9% |
| 1 MiB | XML | 17 | 22,657 | 20,887 | 18,298 | −7.8% | +10.4% |
| 1 MiB | XML | 128 | 23,711 | 21,795 | 19,957 | −8.1% | +8.3% |
| 1 KiB | JSON | 1 | 14,383 | 14,533 | 8,886 | +1.0% | −2.1% |
| 1 KiB | JSON | 16 | 97,999 | 98,045 | 62,587 | +0.0% | +0.4% |
| 1 KiB | JSON | 17 | 102,078 | 103,871 | 63,243 | +1.8% | −1.0% |
| 1 KiB | JSON | 128 | 186,286 | 189,013 | 98,365 | +1.5% | −1.6% |
| 1 KiB | XML | 1 | 14,699 | 15,064 | 9,551 | +2.5% | −3.3% |
| 1 KiB | XML | 16 | 99,558 | 97,505 | 69,440 | −2.1% | +2.1% |
| 1 KiB | XML | 17 | 107,032 | 106,184 | 72,037 | −0.8% | +0.8% |
| 1 KiB | XML | 128 | 191,922 | 192,559 | 109,251 | +0.3% | −0.3% |

The target 10 MiB/c=128 cells did not improve. In JSON, CPU seconds/GiB were
0.27280 (current) and 0.27284 (candidate); in XML they were 0.28677 and 0.29159.
The candidate's 1 MiB/JSON/c=128 result was 12.2% lower in RPS and 15.2% higher
in CPU/GiB. Seven of eight 1 MiB cells had lower candidate RPS. These observations
are sufficient to withhold adoption; they do not establish a universal slowdown
or identify the responsible instruction or cache effect.

At c=16 the candidate does not intentionally select reduced frames; at c=17,
17 connections do not guarantee 17 simultaneous response bodies. Direction
changes near that boundary and differences at c=1 must not be presented as
proof of adaptive sizing. A Rust-level counter or pool optimization is not
justified by this table alone.

### Resource snapshots at the target workload

For 10 MiB/JSON/c=128. RSS and cgroup values are MiB at the end of each cell;
pressure is an interval percentage. No CPU throttling event occurred in any of
the 72 cells.

| Metric | Current | Candidate | fake |
| --- | ---: | ---: | ---: |
| p99 latency, ms | 117.99 | 128.51 | 182.36 |
| CPU seconds/GiB | 0.27280 | 0.27284 | 0.27755 |
| CPU pressure, some / full | 2.599 / 2.579% | 3.354 / 3.285% | 48.617 / 18.167% |
| Process RSS | 101.035 | 66.434 | 68.770 |
| Cgroup total memory | 1,199.602 | 1,173.953 | 1,178.094 |
| Cgroup anonymous memory | 93.582 | 60.461 | 46.043 |
| Cgroup file memory | 1,100.781 | 1,107.863 | 1,100.594 |

The lower candidate RSS here is an observed snapshot, not a peak-memory result.
For example, the single-connection 10 MiB JSON snapshot was higher for candidate
(69.727 MiB) than current (39.504 MiB). Reuse and retention matter separately
from the size of the next frame.

## Mixed and finite cold/warm checks

The mixed workload runs two existing load-generator processes together: JSON
10 MiB at c=16 and JSON 1 KiB at c=16, each across 100 objects. Each pair warms
for two seconds and measures for five seconds. All six measured streams and
six warmup streams exited successfully with zero reported errors. CPU/GiB uses
the combined delivered bytes; it is dominated by the larger objects.

| Metric | Current | Candidate | fake |
| --- | ---: | ---: | ---: |
| 10 MiB RPS | 2,848 | 2,943 | 2,452 |
| 1 KiB RPS | 11,190 | 11,181 | 12,037 |
| 10 MiB p99, ms | 12.54 | 12.38 | 16.40 |
| 1 KiB p99, ms | 5.19 | 5.56 | 7.88 |
| Combined CPU seconds/GiB | 0.25788 | 0.25143 | 0.27296 |
| CPU pressure, some / full | 6.755 / 6.583% | 5.785 / 5.691% | 34.125 / 12.642% |

For a finite cold/warm diagnostic, curl's parallel mode requests every one of
100 distinct 10 MiB objects once, with maximum concurrency 1 or 128. Each cold
scan follows eviction of only that server's stored blobs; a warm scan follows
immediately without eviction. All 1,200 responses had status 200 and exactly
10 MiB, and all curl processes exited successfully. The c=128 scan has only
100 objects, so at most 100 requests can be active. These scans use a different
client and are too short to compare as steady-state RPS with the matrix above.

| Maximum c | Cache | Current elapsed / CPU cost | Candidate elapsed / CPU cost | fake elapsed / CPU cost |
| ---: | --- | ---: | ---: | ---: |
| 1 | Cold | 0.29 s / 0.22774 | 0.30 s / 0.22717 | 0.25 s / 0.21961 |
| 1 | Warm | 0.17 s / 0.16839 | 0.18 s / 0.18017 | 0.21 s / 0.21265 |
| 128 | Cold | 0.17 s / 0.74542 | 0.15 s / 0.45177 | 0.15 s / 0.25758 |
| 128 | Warm | 0.15 s / 0.26659 | 0.14 s / 0.27326 | 0.15 s / 0.15844 |

CPU cost is CPU seconds/GiB. The 10 ms elapsed-time resolution is material for
these short scans. Docker's backing storage may still be cached. The candidate's
lower CPU cost in this one cold parallel scan does not outweigh the larger
matrix's lack of a steady-state target improvement or establish a general
disk-performance benefit.

## Limits

No native Linux-host performance result, Windows executable test, real-cloud
differential run, or external Pub/Sub-emulator run is claimed. Existing recovery
tests passed; this read-only change adds no durable-write transition. The checks
cover concrete cases and do not prove the absence of every possible regression.
Thread-per-core and copy-avoidance transports remain outside this candidate.
