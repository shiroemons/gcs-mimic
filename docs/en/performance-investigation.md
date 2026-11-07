# Finding performance bottlenecks

[日本語](../ja/performance-investigation.md)

Use this guide for a performance investigation, not as a routine feature gate.
Start with the [investigation history](evidence/read-optimization-summary.md).
It separates adopted behavior, rejected candidates, external experiments and
unmeasured ideas so that a later change does not repeat the same investigation.

The commands below describe the process used for the 2026-11 read experiments.
They are operator recipes with explicit prerequisites, not a new automated
benchmark suite. The historical results were recorded separately; this guide
does not claim a new run or a native Linux-host result.

## Start with a question and a controlled workload

Write one question, such as: "Does this change reduce server CPU/GiB for full
10 MiB JSON downloads at c=128 without harming c=1 or small downloads?"
Name the mechanism expected to change: copied bytes, allocations, wakeups,
syscalls or catalog work. Do not choose an implementation from RPS alone.

Record source revision and dirty diff identity, compiler and dependency pins,
binary hash, image digest, architecture, kernel, CPU quota/affinity, worker count,
storage backend, authentication, HTTP/TLS, object count/size, access pattern,
logging and healthchecks. Build and finish functional checks before timing.

Use isolated experimental data and resource names. Never load-test a real bucket
or silently stop other projects, runners or containers. On a shared machine,
record the interference limit instead of selecting only favorable samples.

| Dimension | Read-investigation example | Why it matters |
| --- | --- | --- |
| Object size | 1 KiB, 1 MiB, 10 MiB; 10.5 MiB for tail behavior | Small-request overhead and bulk-copy cost differ |
| Concurrency | 1 and 128; 16/17 for the historical adaptive threshold | Throughput and fairness change with concurrency |
| Protocol | JSON media and XML media separately | Headers and adapter work differ |
| Working set | 100 distinct objects, cyclic access | Repeating one hot object can overstate cache benefit |
| Body | Whole body drained, status and exact length checked | A fast header or truncated response is not a fast transfer |
| Correctness | Independent hashes, unaligned Range and negative cases | Equal byte counts alone cannot detect wrong bytes |
| Runtime | Same quota, worker setting and competing-load policy | More CPU is not automatically better efficiency |

The historical external client is preserved as
[read-load-client.go.txt](evidence/read-load-client.go.txt), SHA-256
`9c36385be49534451fdf62c3d71bbc39dbc71e38efe5370ca0cbc0e79cabbf73`.
It is a closed-loop client with no external Go dependencies. Its original
scratchpad comment describes its origin; the archived text is intentionally
unchanged. It is not a general SDK or overload/load-shedding test.

To build a new Linux client from the repository root, select the architecture
of the measurement VM. Record the actual Go version and resulting hash; a new
build is not assumed to match the historical Go 1.24.5 executable.

```sh
PERF_DIR=$(mktemp -d)
cp docs/en/evidence/read-load-client.go.txt "$PERF_DIR/main.go"
go version
env CGO_ENABLED=0 GOOS=linux GOARCH=arm64 \
  go build -o "$PERF_DIR/loadgen-linux" "$PERF_DIR/main.go"
shasum -a 256 "$PERF_DIR/loadgen-linux"
```

Inside a separate Linux client/observer container, set `LOADGEN` to that binary
and `BASE` to the experimental server origin. Create fixtures only in that
isolated server. The client uses bucket `load-10485760` and 100 objects here:

```sh
"$LOADGEN" -mode seed -base "$BASE" -bucket load-10485760 \
  -objects 100 -size 10485760
"$LOADGEN" -base "$BASE" -bucket load-10485760 -objects 100 \
  -size 10485760 -kind media -c 128 -d 2s
"$LOADGEN" -base "$BASE" -bucket load-10485760 -objects 100 \
  -size 10485760 -kind media -c 128 -d 5s
```

`media` is JSON; `public` is the XML path. Confirm the reference server's public
host matches its reachable origin. Check the seed result, client process exit
and every reported `errs` value. This client can exit zero with request errors.
Do not report a successful performance sample if errors are nonzero.

## Match cache preparation and measure without tracing

Prepare every variant with the same data size/count and the same startup,
eviction and priming sequence. Prime every object, then the target concurrency;
confirm the scan reaches the full working set without errors. Restarting a
process does not necessarily evict the kernel's file cache.

For a cold diagnostic, `vmtouch -e` was used only on an experiment's own blob
directory, followed by a residency check. Never drop the machine's global caches.
Eviction and priming happen before accounting starts. Guest-cache eviction on
Docker Desktop does not prove physical-storage coldness. A short finite cold
scan is a different workload from steady-state warm RPS.

Measure one variant at a time. Use a short fixed warmup and measurement period,
rotate variant order, and retain each result. Three rounds can expose direction
changes, but are not a significance test. If variation spans the proposed gain,
report the result as inconclusive or obtain a better controlled environment.

Keep ordinary timing, `perf stat`, CPU sampling and syscall tracing as separate
runs. Tracing can alter scheduling, cache state and syscall latency. Do not
compare a traced candidate against an untraced baseline as a speedup.

## Account for CPU and memory before attributing a cause

The Linux observer must see the server's PID namespace and its cgroup-v2 path,
while the observer and client remain in different cgroups from the server.
For Docker, the historical observer used `--pid container:SERVER`,
`--cgroupns host`, and the experiment's network. Discover and verify the actual
server PID; PID 1 is correct only when the application is the container's PID 1.

With `SERVER_PID` verified inside the observer, read the following immediately
before and after a measured run, and save both snapshots with their timestamps:

```sh
CGROUP_PATH=$(awk -F: '$1 == 0 { print $3 }' "/proc/$SERVER_PID/cgroup")
test -n "$CGROUP_PATH"
CGROUP_DIR="/sys/fs/cgroup$CGROUP_PATH"
awk '{print $1}' /proc/uptime
sed -n '1,30p' "$CGROUP_DIR/cpu.stat"
sed -n '1,10p' "$CGROUP_DIR/cpu.pressure"
sed -n '1,10p' "$CGROUP_DIR/memory.pressure"
sed -n '1,10p' "$CGROUP_DIR/io.pressure"
sed -n '1p' "$CGROUP_DIR/memory.current"
awk '$1 == "anon" || $1 == "file" {print}' "$CGROUP_DIR/memory.stat"
awk '$1 == "VmRSS:" {print}' "/proc/$SERVER_PID/status"
```

For this client's error-free, fixed-size full-body runs:

```text
delivered_GiB = completed_requests × object_size_bytes / 2^30
server_CPU_seconds = delta(cpu.stat usage_usec) / 1,000,000
CPU_seconds_per_GiB = server_CPU_seconds / delivered_GiB
CPU_PSI_some_percent = delta(cpu.pressure some total) / elapsed_usec × 100
```

Use actual elapsed time, including completion of requests started before the
client deadline. Do not divide requests by exactly five seconds when draining
takes longer. In the historical observer, `/proc/uptime` had about 10 ms
resolution; `date +%s%N` did not provide the assumed nanosecond precision.

Server CPU excludes client/observer CPU and is not whole-host CPU. Inspect
`nr_throttled` and `throttled_usec` deltas as well as utilization. CPU PSI is
wall-time pressure, not CPU utilization, mutex waiting or proof that another
project caused the delay. See the [kernel PSI definition](https://docs.kernel.org/accounting/psi.html).

RSS is process resident memory; cgroup memory also includes charged cache and
other cgroup allocations. A snapshot is not a peak. Shared file pages can be
charged mostly to one cgroup. Do not equate lower charged cache with less memory
needed, or sum worker RSS without accounting for shared mappings. For the
existing constrained-container resource workflow, see
[resource measurement](evidence/dist-resource-measurement.md) and
`scripts/measure-container-resources.sh`.

## Use counters, then CPU call stacks

Use a dedicated Linux profiling image with `perf`, `strace` and optional
`sysstat`/`vmtouch`; pin its identity and tool versions in the result. The
historical disposable observer required `PERFMON`, `SYS_PTRACE`, `SYSLOG` and
`--security-opt seccomp=unconfined`. Those settings belong to the observer, not
the application image. Check local kernel/container policy before assuming
support; do not change global host security settings to make a run succeed.

Set `PERF_OUT` to an existing writable output directory inside the observer.
These commands attach to the verified server process while the workload runs
in the observer's separate cgroup:

```sh
perf stat -p "$SERVER_PID" \
  -e task-clock,context-switches,cpu-migrations,page-faults \
  -o "$PERF_OUT/counters.txt" -- \
  "$LOADGEN" -base "$BASE" -bucket load-10485760 -objects 100 \
  -size 10485760 -kind media -c 128 -d 10s

perf record -e cpu-clock -F 199 --call-graph dwarf,8192 \
  -p "$SERVER_PID" -o "$PERF_OUT/perf.data" -- \
  "$LOADGEN" -base "$BASE" -bucket load-10485760 -objects 100 \
  -size 10485760 -kind media -c 128 -d 10s

perf report -i "$PERF_OUT/perf.data" --stdio --no-children -g none \
  --sort dso,symbol --percent-limit 0.5
perf report -i "$PERF_OUT/perf.data" --stdio --no-children \
  --sort dso,symbol --percent-limit 0.5
```

Check profiler exit, workload exit/errors, lost samples and unresolved symbols.
For a multi-process server, select all existing worker PIDs; attaching only to
an already-running nginx master does not profile its existing workers.

A symbol-enabled Rust build should retain the baseline compiler, optimization,
LTO, allocator and codegen settings, changing debug/strip settings explicitly.
Record both executable hashes. The historical symbol build had different
machine code from the release build, so its samples are not an exact-release
instruction profile. Changing to a debug build also changes the workload.

Read `self` and inclusive costs separately. Inclusive caller percentages overlap
and cannot be summed into a CPU pie. Copy functions on a stack establish a copy
path, not its byte count or memory-bandwidth saturation. Low on-CPU samples in
a mutex function do not show how long threads slept waiting for it.

The historical VM did not support hardware cycles/instructions counters, so
software `cpu-clock` was used. Unsupported or uncounted events are missing
evidence, not zero cache misses. The profile also reported a schedstat warning
and one recording had an out-of-order event; those limitations remain in the
[recorded evidence](evidence/read-frame-concurrency-profile.txt).
See the [perf record reference](https://man7.org/linux/man-pages/man1/perf-record.1.html).

## Confirm the path with syscall tracing

Run syscall tracing separately from throughput timing. In one observer terminal:

```sh
strace -f -p "$SERVER_PID" \
  -e trace=preadv2,pread64,writev,sendfile \
  -e raw=preadv2,pread64,writev,sendfile \
  -o "$PERF_OUT/syscalls.txt"
```

After attachment is confirmed, request one known object in another terminal,
verify its bytes, then interrupt the observer's strace process. Raw arguments
avoid logging payload bytes. Expand the syscall selection if a different backend
uses `read`/`write`; zero observed calls can mean the trace was incomplete.
Already-existing worker processes require their own `-p` arguments, even with
`-f`. Inspect unfinished/resumed calls before mechanically counting trace lines.

Useful questions include: does `preadv2` use RWF_NOWAIT, how often does it miss,
does a blocking read actually follow, and which write operation emits the body?
One EAGAIN is not proof of blocking fallback: in the tail investigation the next
cached attempt succeeded without an observed `pread64`. The adaptive candidate's
separate cold check did observe that fallback.

## Investigate waiting when CPU samples do not explain latency

| Observation | Next discriminator | Do not conclude yet |
| --- | --- | --- |
| High system CPU with copy stacks | Bytes/CPU cost, frame/syscall count, separately measured sharing/copy-avoidance hypothesis | Memory bandwidth is saturated |
| High CPU pressure or throttling | Quota, runnable threads, client CPU and host scheduling | A mutex or another named project is responsible |
| Low CPU but long responses | I/O pressure, per-thread scheduling, application queue/lock instrumentation and client receive behavior | The application has no bottleneck |
| High small-object request cost | Metadata/header allocation and serialization stacks | The same change will accelerate 10 MiB copies |
| Cache misses after writes | Admission/hit/invalidation counts for a mixed workload | Read-only benchmark results predict mixed traffic |

When available, `pidstat -u -w -t -p "$SERVER_PID" 1 10` helps distinguish
CPU use and context switching. Per-thread `/proc/PID/task/TID/schedstat`
counters separate runtime from runnable-queue wait. Match TIDs between snapshots
and disclose newly created or exited threads; subtracting aggregate totals from
different thread populations is invalid. Runnable wait is not mutex or disk
sleep, and summed thread time is not elapsed wall time. See
[scheduler statistics](https://docs.kernel.org/scheduler/sched-stats.html).

## Test the hypothesis and preserve the decision

Change one mechanism in an isolated worktree. Re-run the same uninstrumented
workload with equivalent preparation and include controls where the mechanism
should not matter. A diagnostic counter or trace may explain behavior, but its
instrumented timing is a separate result.

Keep status/body/hash checks, bounds, authorization, checksums and durability.
For read changes, exercise small/empty bodies, exact and fractional frame
boundaries, unaligned Range, held-buffer ownership, concurrent replacement or
deletion, slow clients, cancellation and read errors as relevant. A lower mean
or CPU/GiB does not excuse worse p99 or an unsupported response path.

Save one final evidence set containing the hypothesis, exact measurement inputs,
per-run results/errors, counter units/formulas, sample scope, excluded attempts
with reasons, functional-check scope, decision and a condition for revisiting
it. Report observed ranges alongside medians. Keep external-mechanism results
separate from gcs-mimic results. Do not archive local user paths, credentials or
patches tied to a transient source layout; identify source and binary revisions
and describe the behavior that changed.

Remove only resources created for the experiment after collecting evidence.
Retain an unadopted source worktree if it still contains uncommitted work; do not
delete other workers' state. Update the investigation index so the next reader
starts from the conclusion rather than rerunning the same measurement.
