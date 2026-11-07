# Read-tail framing and kernel-copy investigation

[Investigation index](read-optimization-summary.md) ·
[Measurement and diagnosis guide](../performance-investigation.md)

Measured on 2026-11-08 JST. This is an investigation, not an adopted optimization.

## Decision

Keep the current object reader. Preserving the last full 1 MiB frame reduces a
10 MiB download from 13 file reads to 10, but does not establish an overall
performance improvement. JSON and XML results disagree, and XML at concurrency
one is slower in all three paired samples. No candidate reader change is merged into main.

Kernel-assisted file sending deserves further investigation: in a separate
nginx experiment, `sendfile` reduces server CPU seconds per GiB substantially in
all three large-object samples. This is not an implementation in gcs-mimic.
Single-connection p99 latency worsens in every sample, so it is not a general
latency improvement either.

## Large-object matrix

The following are medians of three 5-second measured runs, each preceded by a
2-second warmup. Parentheses show the observed minimum and maximum RPS. All
samples are retained, including the unusually slow baseline JSON sample.
There are too few samples, and too much variation, to claim a small gain.

| 10 MiB download | Current RPS | Tail candidate RPS | Candidate change | fake-gcs-server RPS |
| --- | ---: | ---: | ---: | ---: |
| JSON, c=1 | 464 (455–484) | 451 (402–492) | -2.8% | 386 (334–401) |
| XML, c=1 | 510 (506–553) | 458 (453–540) | -10.2% | 410 (399–414) |
| JSON, c=128 | 2,883 (1,438–2,965) | 2,814 (2,628–3,180) | -2.4% | 2,692 (1,815–2,748) |
| XML, c=128 | 2,610 (2,529–3,019) | 2,785 (2,604–3,024) | +6.7% | 2,476 (2,084–2,919) |

CPU seconds per GiB measures server work per transferred byte; lower is better.
It is the CPU usage delta of the server cgroup divided by successfully
transferred bytes. It excludes the load generator and is not whole-host CPU.

| 10 MiB download | Current CPU s/GiB | Tail candidate CPU s/GiB | fake-gcs-server CPU s/GiB |
| --- | ---: | ---: | ---: |
| JSON, c=1 | 0.211338 | 0.219606 | 0.267321 |
| XML, c=1 | 0.195451 | 0.216351 | 0.252217 |
| JSON, c=128 | 0.254658 | 0.269180 | 0.284130 |
| XML, c=128 | 0.288509 | 0.266370 | 0.294358 |

## GCS control matrix

These conditions use one 5-second sample each. They exercise unchanged small
selections and fractional tails. Their mixed differences also caution against
attributing every RPS change to framing. They are not statistical proof of a
regression or an improvement.

| Size | Route | Concurrency | Current RPS | Tail candidate RPS | Change | fake-gcs-server RPS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | JSON | 1 | 11,559 | 11,359 | -1.7% | 9,354 |
| 1 KiB | XML | 1 | 10,853 | 11,409 | +5.1% | 9,637 |
| 1 KiB | JSON | 128 | 166,394 | 170,680 | +2.6% | 97,469 |
| 1 KiB | XML | 128 | 167,480 | 172,000 | +2.7% | 116,703 |
| 1 MiB | JSON | 1 | 2,766 | 2,409 | -12.9% | 2,370 |
| 1 MiB | XML | 1 | 2,320 | 2,833 | +22.1% | 2,760 |
| 1 MiB | JSON | 128 | 18,744 | 19,485 | +4.0% | 19,360 |
| 1 MiB | XML | 128 | 20,475 | 19,473 | -4.9% | 19,457 |
| 10.5 MiB | JSON | 1 | 495 | 505 | +2.0% | 333 |
| 10.5 MiB | XML | 1 | 470 | 536 | +14.0% | 299 |
| 10.5 MiB | JSON | 128 | 2,602 | 2,697 | +3.7% | 2,414 |
| 10.5 MiB | XML | 128 | 2,703 | 2,787 | +3.1% | 2,419 |

All 92 measured cells completed successfully: 72 GCS cells with 5,809,104
requests and 20 separate nginx cells with 2,194,448 requests, totaling
8,003,552 requests with zero reported errors. All per-cell
warmups also reported zero errors. The load generator checks HTTP status,
body-read errors and exact response length; separate checks compare full bytes.

The [complete measured samples](read-tail-copy.tsv) retain per-run RPS, request
counts, p99, server CPU deltas, CPU pressure, end RSS and cgroup memory.
CPU pressure is a saved derived value: the table does not retain its original
pressure-total deltas or elapsed intervals, so it cannot be independently
recomputed from this archive. CPU/GiB can be recomputed from the retained inputs.
`media` denotes the GCS JSON media route; `public` denotes the GCS XML route or
the nginx static route, depending on the experiment column. The `tail` rows
contain the gcs-mimic/fake-gcs-server comparison. No measured sample is excluded.

## Separate copy-avoidance experiment

This comparison uses nginx 1.31.3 with eight workers, the same read-only data
volume and configurations differing in exactly one directive: `sendfile on`
versus `sendfile off`. Both use `output_buffers 1 1m`,
`sendfile_max_chunk 1m`, `read_ahead 0`, disabled access logging, and disabled
gzip, AIO, direct I/O and TCP corking. The
[`read_ahead` directive](https://nginx.org/en/docs/http/ngx_http_core_module.html#read_ahead)
does not mean all Linux kernel read-ahead was disabled. TCP_NODELAY is enabled. Open-file caching
and keepalive settings are identical. The chunk limit is an upper bound on a
sendfile call, not a guarantee of identical syscall sizes.

The [off configuration](nginx-sendfile-off.conf.txt) is archived verbatim; the
on configuration changes only `sendfile off;` to `sendfile on;`.

| 10 MiB static HTTP | sendfile off | sendfile on | Change |
| --- | ---: | ---: | ---: |
| c=1, median RPS | 483 | 511 | +5.8% |
| c=1, median CPU s/GiB | 0.209743 | 0.076733 | -63.4% |
| c=1, median per-run p99 | 3.05 ms | 3.63 ms | +19.0%, worse |
| c=128, median RPS | 2,656 | 3,771 | +42.0% |
| c=128, median CPU s/GiB | 0.270961 | 0.066919 | -75.3% |

At c=128, off ranged from 2,070 to 2,659 RPS and on ranged from 2,680 to
4,065 RPS. Every paired sample used less server CPU with sendfile. This is
evidence for the mechanism in this environment, not a forecast for gcs-mimic.
The static server does not perform GCS authorization, metadata selection or
generation handling. No HTTPS or gRPC result is implied.

Tracing all existing nginx workers confirmed file-read-plus-write calls when
off and sendfile calls when on. One full 10 MiB response used 10 `pread64` calls
and 17 `writev` calls when off; when on it used 13 `sendfile` calls and one
header `writev`. Short sendfile results were observed. Full-body and unaligned
Range bytes matched the source file in both modes, with HTTP 206 and the
expected Content-Range for the Range requests. Tracing was not active during
timed measurements.

Shared page-cache charging makes the nginx cgroup memory figures incomparable:
the on cgroup was charged approximately 1.08 GiB of file pages while the off
cgroup was charged approximately 45 KiB. Both accessed the same pages. Do not
interpret that difference as sendfile memory overhead or a memory saving.

The small-size controls below are single samples, with the same limitations as
the GCS control matrix. In particular, sendfile is not universally beneficial.

| Static HTTP size | Concurrency | Off RPS | On RPS | Off CPU s/GiB | On CPU s/GiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | 1 | 15,565 | 15,069 | 22.560168 | 25.270655 |
| 1 KiB | 128 | 177,088 | 160,672 | 14.189076 | 17.005882 |
| 1 MiB | 1 | 1,947 | 4,340 | 0.486124 | 0.099635 |
| 1 MiB | 128 | 12,872 | 30,209 | 0.587656 | 0.107547 |

## What the reader candidate changes

The reader remembers the selected transfer length. For selections larger than
1 MiB, a final full 1 MiB remains a single frame. Selections at most 1 MiB and
fractional tails continue using the existing 256 KiB pool. Range changes update
the selection length, and prefix peeking restores it. Reader construction uses
the existing shared constructor. No dependency, unsafe code, checksum,
authorization, durability, admission or buffer-limit change is introduced.

| Full response | Current read calls | Candidate read calls |
| --- | ---: | ---: |
| 1 MiB | 4 | 4 |
| 10 MiB | 13 | 10 |
| 10.5 MiB | 12 | 12 |

These counts were observed with syscall tracing on warm files. Short reads and
cold-cache fallback can legitimately change the count. Both implementations
still copy the same total payload into user buffers and into the socket path;
fewer calls do not by themselves remove the dominant copies.

## Environment and controls

- Apple M4 Pro, 14 logical CPUs, 64 GiB host memory; Docker Desktop Linux
  7.0.14-linuxkit, ARM64, 14 virtual CPUs and approximately 15.6 GiB VM memory.
- Plain HTTP/1.1 keepalive, separate server and client containers, no payload
  compression, 100 objects per size, full response bodies drained and checked
  for status and exact length. Each size uses an incompressible seeded payload.
- Sizes: 1 KiB, 1 MiB, 10 MiB and 10.5 MiB; c=1 and c=128; JSON and XML.
  The nginx experiment uses the first three sizes through its static route.
- Current and candidate use eight Tokio workers and separate disk-backed
  volumes. Both container healthchecks are disabled. fake-gcs-server uses its
  default runtime settings with `-log-level error`, avoiding access-log costs.
- Only experimental blob caches were evicted. Every GCS server was primed with
  the same c=1 and c=128 workload for all sizes before timing. Residency checks
  confirmed all payload pages resident. The nginx shared data volume was
  evicted once, then primed through both servers.
- Rust builds, tests, payload validation and syscall tracing completed before timing.
  Experimental servers were measured sequentially. Other projects and runners
  were not stopped. The shared host remains a source of uncertainty; the cause
  of the slow baseline sample was not established.
- GCS target runs rotated current/candidate/fake order across three rounds.
  nginx target rounds used off/on, on/off, off/on. Smaller and fractional-size
  controls use one sample each and are exploratory, not regression guarantees.
- RSS is an end-of-run process snapshot, not a peak. Cgroup memory includes
  charged cache pages and is recorded separately. nginx process RSS is omitted
  because summing worker RSS would double-count shared mappings.

## Verification

The final candidate was checked before tests. No SDK or cross-target result
from an earlier candidate is counted here.

| Check | Result |
| --- | --- |
| `cargo check --locked --workspace --tests` | Passed on native macOS |
| Linux ARM64 musl candidate check and release build | Passed |
| `cargo test --locked --workspace` | 2,948 passed, 0 failed, 1 existing opt-in Pub/Sub test ignored; exit 0 |
| `cargo clippy --locked --workspace --all-targets --offline -- -D warnings` | Passed |
| `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| `cargo fmt --all -- --check` and `git diff --check` | Passed |
| Evidence Markdown lint | 0 issues |
| Linux candidate full 10 MiB body comparisons | 128 JSON and 128 XML responses matched the reference bytes, at concurrency 16 |
| Linux candidate unaligned Range | JSON and XML bytes 123–3670138 matched the expected 3.5 MiB slice, status 206 |
| Linux candidate after cache eviction | 0 of 550,500 blob pages resident before the request; full 10 MiB bytes matched |
| Independent Rust review | No blocking correctness or Rust-structure finding |

The Rust total includes 1,750 library tests, 31 binary tests, 938 application
integration tests, one code-generation test, 209 xtask unit tests, 15 xtask
integration tests and four doctests. The additional focused reader/body runs
are not counted twice. The unchanged opt-in Pub/Sub test requires an explicitly
started local emulator.

The new tests cover retained frame ownership and exact bytes, a full final
frame after prefix peeking, an unaligned fractional tail, resetting to a small
selection and an empty selection. Existing full-workspace tests exercise
authorization, generation selection, memory storage, gRPC, error handling,
cancellation, concurrency, restart and recovery.

After the performance runs, the cache-eviction check observed one EAGAIN and
then successful cached reads. It did not observe a blocking `pread64` fallback,
so this specific Linux check does not establish that fallback path. Native
reader tests exercise blocking reads. Evicting guest pages does not establish
physical-disk latency because Docker Desktop backing layers may remain cached.

Official SDK suites, Windows runtime checks, other Linux target checks, native
Linux-host performance and a gcs-mimic sendfile implementation were not run for
this candidate. The candidate is retained only as investigation material.
Only the named experimental containers, volumes and network were removed after
measurement; other workloads were left running. No experimental reader or
transport change was merged into main.

## Integration constraints and other candidates

The current `ObjectBody` yields `Bytes`, and Hyper owns socket writes through
the generic asynchronous transport. A file descriptor and offset are not a
substitute for a `Bytes` frame. A robust sendfile integration must preserve
authorization and the already-selected generation, handle partial sends and
EAGAIN, retain file lifetime through consumption, respect backpressure and
cancellation, and keep a correct path for cold I/O, TLS, memory storage and
other platforms. No transport bypass was added for this experiment.

This is a transport API constraint, not a Rust-language limitation. The
existing `libc` dependency exposes Linux `sendfile`. nginx keeps file descriptor
and range information in its output chain, writes memory-backed headers with
`writev`, and calls `sendfile` for file-backed buffers. It updates offsets after
partial sends, waits for readiness after EAGAIN, retries EINTR, and rejects an
unexpected zero-byte result. Its optional thread-pool path also performs the
sendfile call away from the event worker. The corresponding implementations are
the pinned
[static module](https://github.com/nginx/nginx/blob/release-1.31.3/src/http/modules/ngx_http_static_module.c)
and
[Linux send chain](https://github.com/nginx/nginx/blob/release-1.31.3/src/os/unix/ngx_linux_sendfile_chain.c).
The installed Hyper 1.12.0 write interface accepts byte slices and vectored byte
slices, without a file-transfer operation. Reproducing the syscall in Rust
alone therefore would not measure a gcs-mimic download through its current HTTP
stack. Its integration and performance remain unverified.

A bounded immutable-payload cache could avoid both page-cache lookup and the
file-to-user copy on hits. It was not implemented or timed here. The 100-object
10 MiB working set is approximately 1 GiB; a small LRU will not retain this
entire cyclic workload. Any cache budget must include bytes still held by
in-flight responses after eviction, and preserve generation and policy checks.

Read-ahead hints were researched but not timed. The measured workload is fully
cached, so additional prefetching does not remove its copies. Linux sequential
and random advice changes the read-ahead state for the open file; applying it
to a cached shared descriptor needs care when full and Range reads interleave.
Native Linux cold-storage measurements are needed before selecting that path.

Thread-per-core remains deferred pending a dedicated/native Linux comparison.

The mechanisms and restrictions were checked against
[Linux sendfile documentation](https://man7.org/linux/man-pages/man2/sendfile.2.html),
[Linux fadvise documentation](https://man7.org/linux/man-pages/man2/posix_fadvise.2.html)
and [nginx core documentation](https://nginx.org/en/docs/http/ngx_http_core_module.html).

## Revisions

The worktree starts at `2c2e2368a9367e729ca2f08b23677638537ede24`.
The reusable baseline executable was built at
`9a823d3028b65e0212e0799b6335be3b765ab062`; `src`, the application manifests and
lockfile, Dockerfile and toolchain file are identical between these revisions.
The candidate is the uncommitted reader diff in this investigation worktree.
Rust 1.99.1 was verified against the official stable channel manifest. Both
executables use the existing ARM64 musl release configuration, mimalloc,
size optimization and fat LTO.

| Input | SHA-256 or pinned identity |
| --- | --- |
| Baseline executable | `4288b95d957d12eaa9d31d62464fee8e77b5cca34b4c6227b3c6bd3485e681f6` |
| Candidate executable | `77ed56bc5e04a78136f648f0e54aa8958d39c6860dddc6d5e2ebe674d515a200` |
| Baseline container image | `sha256:a17546d6cd070a676f6b891df157fdafc035d4a3f0b1fc52a54d4ad93a11bdf1` |
| fake-gcs-server | `1.56.1`, image digest `sha256:797ce226d62f947c009dc40246b30cfb456b8473d8241407f9d6f2c04e4d69ef` |
| nginx image | `nginx@sha256:4a73073bd557c65b759505da037898b61f1be6cbcc3c2c3aeac22d2a470c1752` |
| Existing external Go client source | `9c36385be49534451fdf62c3d71bbc39dbc71e38efe5370ca0cbc0e79cabbf73` |
| External client executable | `6cb22ae3fd4f8301d4d05fd52c1fac36fc09023cbb4d99c447445138a3fdf462` |

No feature-ledger status is changed. This investigation does not establish a
new implemented API behavior or release-performance guarantee.
