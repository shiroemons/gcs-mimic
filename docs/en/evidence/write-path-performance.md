# Write path performance investigation

This records the 2026-11-12 investigation of uploads and deletes. Read it before
repeating a rejected experiment. Decisions apply to the recorded workloads and
environments. For reads, start from the
[read optimization summary](read-optimization-summary.md).

## Environments and method

| Item | Native macOS | Container |
| --- | --- | --- |
| Host | Apple M4 Pro, macOS 27.2, internal SSD with APFS | Same host, Docker Desktop 29.9.2 |
| Build | Release build of the measured commit | Repository `Dockerfile`, linux/arm64 |
| Data | A new directory on the system volume per run | A new named volume per run |
| Host load | Load average 5.4 to 7.3 | Load average 4.7 to 10.7 |

Other projects' containers kept running. The load client is a closed-loop
HTTP/1.1 keep-alive client derived from
[read-load-client.go.txt](read-load-client.go.txt), with upload, delete and
metadata patch modes; it is not archived. Every reported cell checked each
response's status and length and had no errors. Each cell is a single short
run; comparisons alternate builds within one session. Objects are 1 KiB unless
stated.

## Findings and decisions

| Question | Finding | Decision and reason to revisit |
| --- | --- | --- |
| What limited concurrent uploads in a container? | One catalog writer synced the WAL for every upload. In a traced run that sync averaged about 0.4 ms of the roughly 0.56 ms each upload held the writer | Adopted group commit. Revisit if writes other than single-request uploads dominate a workload |
| Why were native macOS uploads about ten times slower? | `sync_all` is `F_FULLFSYNC` on Apple platforms: 3.9 to 4.6 ms median, and 268 per second in total from 8 threads. Each upload paid two | Adopted `F_BARRIERFSYNC`, 0.24 to 0.26 ms median, for blobs and catalog directories |
| Should each catalog commit be flushed with `PRAGMA fullfsync`? | Uploads became 2 to 5 times faster than before, but metadata updates went from 0.12 to 4.9 ms and deletes from 5.9 to 10.5 ms | Rejected: it slows every write that is not an upload |
| Should the advisory upload check read on the pool? | The check then passed while an external lock held the writer, so uploads streamed and committed after their 503; integration tests require the check to queue | Rejected |
| Does every upload need SHA-256? | Only upload replay compares it. Server CPU for 1,000 uploads of 100 KiB fell from 1,280-1,300 ms to 1,170-1,230 ms without it | Adopted: hashed only with an idempotency token |
| Should reads keep cached answers across unrelated writes? | In a container, one connection writing to another bucket cut 100 KiB reads from 38,858 to 26,858 per second at 8 connections and from 83,142 to 41,308 at 32 | Deferred: the remaining rate is far above local-development load, and a missed dependency would return stale data. Revisit when a real workload shows slow reads during writes |

## Results

Native macOS, three alternating runs of each build. The barrier build is the
final source; metadata updates and reads are unchanged within run variation.

| Operation | Before | After |
| --- | --- | --- |
| Upload, 1 connection, median latency | 12.9 to 13.8 ms | 0.93 to 0.96 ms |
| Upload, 8 connections | 106 to 109 per second | 2,530 to 2,910 per second |
| Upload, 32 connections | 117 to 119 per second | 2,950 to 3,009 per second |
| Overwrite, 8 connections | 94 to 101 per second | 1,935 to 2,074 per second |
| Delete, 1 connection, median latency | 5.87 to 5.89 ms | 0.51 to 0.55 ms |
| Metadata update, 1 connection, median latency | 0.12 ms | 0.12 to 0.14 ms |

Container, 3,000 uploads per cell, three alternating runs of the group commit
build against its parent. Medians of the three runs:

| Operation | Before | After |
| --- | --- | --- |
| Upload, 8 connections | 1,473 per second | 2,060 per second |
| Upload, 32 connections | 1,620 per second | 2,797 per second |
| Overwrite, 8 connections | 915 per second | 1,452 per second |

## Not measured

- A Linux host outside Docker Desktop, and the container build after the macOS
  change, whose Linux code is unchanged.
- The wall time of an SDK test suite against each build.
- Hop reduction on the upload path: about a quarter of a container upload's
  CPU samples woke another thread, but uploads were limited by syncs, not CPU.
