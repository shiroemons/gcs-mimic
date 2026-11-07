# Read path performance

<!-- markdownlint-configure-file {"MD013": false} -->

Measured on 2026-11-07. This change targets object reads, whose rate was far below
fake-gcs-server's at local scale. These are observations from one host, not a gate.

Later frame-size experiments, CPU profiling, copy-avoidance research and their
decisions are indexed in [read optimization investigations](read-optimization-summary.md).
Use [finding performance bottlenecks](../performance-investigation.md) for the
measurement and diagnosis procedure.

## Scope

Three GET requests, each over HTTP/1.1 with keep-alive, without TLS and with permissive
authentication:

| Request | Path |
| --- | --- |
| Metadata | `/storage/v1/b/{bucket}/o/{object}` |
| Media (JSON) | `/storage/v1/b/{bucket}/o/{object}?alt=media` |
| Media (XML) | `/{bucket}/{object}` |

Buckets held 1,000 objects of 1 KiB, 100 KiB or 1 MiB, or 100 objects of 10 MiB, and each run
requested every object in turn. Uploads, listing, mixed reads and writes, strict authentication,
gRPC, TLS, HTTP/2, range and conditional requests, native binaries, linux/amd64 and other hosts
were not measured.

## Setup

- The host was macOS on Apple silicon. Docker Desktop's VM had 14 vCPUs and ran kernel
  `7.0.14-linuxkit` for linux/arm64, with transparent huge pages set to `always`.
- The servers were `fsouza/fake-gcs-server:1.56.1` (`-scheme http`, with its filesystem root on a
  volume), `ghcr.io/shiroemons/gcs-mimic:0.12.0`, and this change built from the repository
  `Dockerfile`. Each ran in its own container on one Docker network with a named volume and default
  settings. gcs-mimic also had `GCS_MIMIC_SCHEME=http`. All servers ran at once, and only the one
  under load was busy.
- The load came from a closed-loop Go client kept outside the repository, in a container on the
  same network. Each of `c` connections sent GETs back to back for 3 seconds and read every body.
  Latency runs from sending a request to its last body byte. Each cell ran three rounds, with the
  servers taking turns within a round, and the tables give the median of each metric. Every
  response returned status 200 and the expected length.
- Process memory is `VmRSS` from `/proc/1/status`. Container memory is `docker stats`, which
  excludes inactive page cache. The two are kept separate.

## What changed

| Change | Why |
| --- | --- |
| Reads run on up to eight read-only WAL connections, each on its own thread | Every catalog read queued on the one writer thread: 0.12.0 stays between 20,000 and 27,000 metadata reads per second from 8 to 128 connections |
| A read cache keeps records, public readability and open files of live objects read with permissive access, until any catalog write | A hit skips two thread wakeups and the SQL snapshot |
| Object bytes come from the page cache with `preadv2(RWF_NOWAIT)` on the async worker, falling back to a blocking read | A blocking read costs two thread wakeups, which is more than the copy |
| The connection pulls the response body instead of a task pushing it through a channel | One task switch per chunk goes away, and only one frame is in flight |
| Frames are 1 MiB while more than 1 MiB remains, then up to 256 KiB. A read smaller than a pooled size gets a buffer of exactly its size | Fewer frames for large objects, without a small object holding a large buffer |
| `TCP_NODELAY` on accepted connections | Nagle's algorithm held the last segment of each response until a delayed acknowledgement, about 40 ms |
| The default worker count is the available CPUs, from 2 to 8, instead of 2 | Two workers capped concurrent downloads |
| musl builds use `mimalloc` as the global allocator | musl's allocator takes one global lock |
| The musl binary disables transparent huge pages for itself with `prctl(PR_SET_THP_DISABLE)` | With huge pages set to `always`, the allocator's regions became resident 2 MiB at a time |

## Throughput and latency

Each cell gives requests per second, then p50, p95 and p99 latency in microseconds. Higher is
better for the first value and lower for the others. "Behind fake-gcs-server" lists the metrics
where this change did worse. For 1 MiB and 10 MiB, the same build ran a second time in the same
rounds, to show how far one build varies.

### 1 KiB

| Request | c | fake-gcs-server | 0.12.0 | This change | Behind fake-gcs-server |
| --- | ---: | --- | --- | --- | --- |
| Metadata | 1 | 9,250 / 101 / 143 / 202 | 7,378 / 130 / 174 / 231 | 14,868 / 64 / 81 / 113 | none |
| Metadata | 8 | 37,200 / 193 / 396 / 598 | 20,647 / 378 / 506 / 605 | 64,538 / 112 / 204 / 351 | none |
| Metadata | 32 | 87,882 / 263 / 975 / 1,465 | 19,939 / 1,583 / 1,885 / 2,241 | 157,016 / 160 / 468 / 857 | none |
| Metadata | 128 | 107,399 / 722 / 3,792 / 5,897 | 26,287 / 4,735 / 5,845 / 6,778 | 183,608 / 412 / 2,111 / 3,853 | none |
| Media (JSON) | 1 | 9,365 / 100 / 139 / 207 | 24 / 41,906 / 43,883 / 44,164 | 14,459 / 67 / 80 / 123 | none |
| Media (JSON) | 8 | 39,867 / 176 / 388 / 619 | 193 / 41,881 / 43,914 / 45,359 | 63,805 / 113 / 204 / 397 | none |
| Media (JSON) | 32 | 86,033 / 243 / 1,085 / 1,693 | 768 / 41,867 / 44,197 / 46,140 | 155,845 / 158 / 484 / 918 | none |
| Media (JSON) | 128 | 99,669 / 731 / 4,483 / 6,986 | 3,065 / 41,717 / 43,142 / 51,149 | 186,387 / 423 / 2,166 / 3,912 | none |
| Media (XML) | 1 | 9,753 / 96 / 134 / 199 | 24 / 42,045 / 43,206 / 43,452 | 14,900 / 65 / 77 / 115 | none |
| Media (XML) | 8 | 41,913 / 169 / 368 / 586 | 191 / 41,958 / 43,986 / 46,181 | 65,314 / 111 / 200 / 381 | none |
| Media (XML) | 32 | 96,734 / 210 / 992 / 1,581 | 764 / 41,899 / 44,435 / 45,988 | 160,167 / 155 / 478 / 885 | none |
| Media (XML) | 128 | 111,994 / 652 / 3,909 / 6,246 | 3,048 / 41,854 / 43,335 / 57,186 | 192,284 / 407 / 2,133 / 3,847 | none |

0.12.0 answered every small download in about 42 ms: the Nagle delay that `TCP_NODELAY` removes.

### 100 KiB

| Request | c | fake-gcs-server | 0.12.0 | This change | Behind fake-gcs-server |
| --- | ---: | --- | --- | --- | --- |
| Metadata | 1 | 9,300 / 101 / 140 / 205 | 7,524 / 129 / 162 / 229 | 15,000 / 64 / 78 / 113 | none |
| Metadata | 8 | 37,731 / 190 / 385 / 585 | 21,126 / 371 / 488 / 574 | 65,339 / 112 / 200 / 334 | none |
| Metadata | 32 | 93,584 / 240 / 940 / 1,475 | 20,961 / 1,505 / 1,764 / 2,155 | 158,992 / 157 / 475 / 859 | none |
| Metadata | 128 | 112,373 / 655 / 3,611 / 5,702 | 26,688 / 4,741 / 5,464 / 6,428 | 195,589 / 400 / 2,117 / 3,865 | none |
| Media (JSON) | 1 | 7,637 / 122 / 180 / 278 | 3,516 / 270 / 369 / 452 | 9,146 / 104 / 129 / 189 | none |
| Media (JSON) | 8 | 31,495 / 207 / 552 / 776 | 5,972 / 1,325 / 1,646 / 1,950 | 51,405 / 141 / 238 / 465 | none |
| Media (JSON) | 32 | 56,530 / 389 / 1,560 / 2,389 | 6,179 / 5,069 / 6,010 / 6,922 | 100,492 / 253 / 712 / 1,245 | none |
| Media (JSON) | 128 | 56,775 / 1,105 / 8,955 / 13,515 | 7,047 / 17,574 / 22,271 / 24,817 | 127,056 / 740 / 2,812 / 4,654 | none |
| Media (XML) | 1 | 8,048 / 115 / 170 / 264 | 3,122 / 304 / 400 / 471 | 9,817 / 99 / 119 / 171 | none |
| Media (XML) | 8 | 33,032 / 197 / 534 / 752 | 5,782 / 1,328 / 1,602 / 2,039 | 51,569 / 140 / 238 / 458 | none |
| Media (XML) | 32 | 57,475 / 383 / 1,542 / 2,450 | 5,929 / 5,293 / 6,259 / 8,153 | 110,559 / 233 / 641 / 1,129 | none |
| Media (XML) | 128 | 59,421 / 1,014 / 8,622 / 13,196 | 6,399 / 19,430 / 22,118 / 25,946 | 131,291 / 710 / 2,791 / 4,844 | none |

### 1 MiB

| Request | c | fake-gcs-server | 0.12.0 | This change | Same build again | Behind fake-gcs-server |
| --- | ---: | --- | --- | --- | --- | --- |
| Metadata | 1 | 8,984 / 104 / 146 / 208 | 7,372 / 130 / 177 / 239 | 14,941 / 64 / 77 / 107 | 14,928 / 64 / 76 / 119 | none |
| Metadata | 8 | 38,047 / 191 / 380 / 570 | 20,939 / 374 / 498 / 614 | 64,915 / 112 / 200 / 333 | 65,281 / 112 / 199 / 326 | none |
| Metadata | 32 | 94,298 / 245 / 917 / 1,389 | 20,691 / 1,524 / 1,835 / 2,193 | 162,807 / 154 / 459 / 824 | 161,520 / 153 / 461 / 846 | none |
| Metadata | 128 | 115,500 / 674 / 3,530 / 5,615 | 26,074 / 4,776 / 5,733 / 6,694 | 191,828 / 404 / 2,130 / 3,848 | 194,148 / 400 / 2,077 / 3,681 | none |
| Media (JSON) | 1 | 2,970 / 322 / 455 / 557 | 872 / 1,094 / 1,410 / 1,716 | 3,328 / 288 / 390 / 480 | 3,598 / 266 / 360 / 455 | none |
| Media (JSON) | 8 | 14,862 / 465 / 997 / 1,414 | 1,739 / 4,632 / 6,467 / 7,673 | 18,412 / 395 / 679 / 963 | 19,364 / 375 / 639 / 926 | none |
| Media (JSON) | 32 | 21,640 / 1,145 / 3,841 / 5,953 | 1,785 / 17,714 / 21,044 / 23,769 | 25,485 / 1,111 / 2,425 / 4,266 | 26,327 / 1,045 / 2,478 / 4,531 | none |
| Media (JSON) | 128 | 21,274 / 3,060 / 23,702 / 34,975 | 1,826 / 68,072 / 82,725 / 87,193 | 26,426 / 4,375 / 9,695 / 13,617 | 26,699 / 4,301 / 9,717 / 13,930 | p50 |
| Media (XML) | 1 | 3,142 / 300 / 423 / 517 | 924 / 1,069 / 1,197 / 1,399 | 3,463 / 282 / 351 / 430 | 3,759 / 260 / 311 / 401 | none |
| Media (XML) | 8 | 15,174 / 456 / 976 / 1,382 | 1,687 / 4,745 / 6,723 / 7,825 | 18,550 / 392 / 676 / 928 | 19,447 / 376 / 639 / 882 | none |
| Media (XML) | 32 | 21,859 / 1,124 / 3,794 / 5,963 | 1,764 / 17,926 / 20,746 / 23,768 | 26,567 / 1,071 / 2,276 / 4,138 | 27,111 / 1,003 / 2,292 / 4,300 | none |
| Media (XML) | 128 | 22,161 / 2,917 / 22,451 / 33,053 | 1,799 / 68,956 / 77,708 / 82,665 | 26,991 / 4,244 / 9,605 / 13,535 | 27,351 / 4,154 / 9,597 / 13,568 | p50 |

### 10 MiB

| Request | c | fake-gcs-server | 0.12.0 | This change | Same build again | Behind fake-gcs-server |
| --- | ---: | --- | --- | --- | --- | --- |
| Metadata | 1 | 9,240 / 101 / 145 / 201 | 7,440 / 129 / 174 / 236 | 14,997 / 64 / 75 / 112 | 14,856 / 64 / 82 / 121 | none |
| Metadata | 8 | 38,257 / 189 / 376 / 576 | 20,240 / 381 / 527 / 618 | 65,793 / 111 / 198 / 328 | 64,789 / 112 / 202 / 336 | none |
| Metadata | 32 | 97,557 / 233 / 907 / 1,459 | 20,812 / 1,514 / 1,771 / 2,218 | 161,960 / 155 / 464 / 850 | 159,740 / 157 / 463 / 842 | none |
| Metadata | 128 | 115,358 / 664 / 3,499 / 5,533 | 25,324 / 4,807 / 5,886 / 6,567 | 187,834 / 401 / 2,156 / 3,996 | 191,194 / 402 / 2,192 / 4,060 | none |
| Media (JSON) | 1 | 465 / 2,059 / 2,788 / 3,083 | 114 / 8,389 / 10,074 / 11,388 | 447 / 2,178 / 2,844 / 3,501 | 477 / 2,025 / 2,630 / 3,549 | rps, p50, p95, p99 |
| Media (JSON) | 8 | 2,474 / 2,966 / 5,112 / 6,560 | 212 / 37,139 / 42,210 / 49,223 | 2,564 / 2,896 / 4,290 / 5,447 | 2,642 / 2,846 / 4,173 / 6,061 | none |
| Media (JSON) | 32 | 2,805 / 10,376 / 22,294 / 29,816 | 296 / 107,097 / 115,262 / 125,450 | 2,792 / 10,548 / 21,653 / 28,469 | 2,821 / 10,454 / 20,953 / 26,196 | rps, p50 |
| Media (JSON) | 128 | 2,945 / 37,468 / 96,028 / 138,906 | 297 / 426,436 / 477,093 / 534,938 | 2,719 / 41,095 / 100,518 / 138,846 | 2,587 / 42,074 / 105,045 / 142,461 | rps, p50, p95 |
| Media (XML) | 1 | 437 / 2,059 / 3,172 / 4,025 | 119 / 8,337 / 8,951 / 9,514 | 457 / 2,165 / 2,496 / 2,879 | 492 / 2,007 / 2,283 / 2,681 | p50 |
| Media (XML) | 8 | 2,560 / 2,900 / 4,854 / 6,432 | 213 / 37,065 / 42,789 / 48,540 | 2,550 / 2,885 / 4,333 / 6,144 | 2,653 / 2,821 / 4,052 / 4,963 | rps |
| Media (XML) | 32 | 2,883 / 10,072 / 22,104 / 29,471 | 296 / 106,761 / 116,517 / 133,732 | 2,761 / 10,677 / 21,630 / 27,422 | 2,893 / 10,227 / 20,586 / 26,173 | rps, p50 |
| Media (XML) | 128 | 3,031 / 35,210 / 100,534 / 145,764 | 302 / 420,150 / 457,735 / 513,645 | 2,749 / 40,195 / 99,140 / 136,754 | 2,846 / 38,762 / 95,052 / 129,880 | rps, p50 |

### Reading the results

39 of the 48 cells are better on all four metrics, and 17 of the 192 metrics are behind, all for
objects of 1 MiB or more.

- One build varies by more than many of the gaps. The second run of the same build turned the
  10 MiB rate for JSON at one connection, and for XML at 8 and 32 connections, from behind to ahead
  of fake-gcs-server. Across the runs of this work, fake-gcs-server itself served 10 MiB JSON at
  128 connections at 1,572 to 2,945 requests per second.
- At 1 MiB and 128 connections only p50 is behind, and the mean latency is lower. By Little's law
  the mean is the connections over the rate: 4.8 ms for this change against 6.0 ms for
  fake-gcs-server with JSON. fake-gcs-server's median is lower because it serves requests
  unevenly: its p99 is 35 ms against 13.6 ms.
- At 10 MiB and 128 connections both runs of this change were 6 to 12% behind in rate. At that
  concurrency the 1 MiB frames in flight take 128 MiB, which no CPU cache holds. In an earlier run
  of 10 MiB objects at 32 connections, 256 KiB frames reached 2,635 requests per second and 1 MiB
  frames 2,529. Shrinking frames as concurrency rises was not tried.

### One 10 MiB download at a time

These runs used the build before the review fixes. Its read path for 10 MiB objects held in the
page cache is the same as this change's. Each cell gives requests per second.

| Configuration | JSON, c=1 | XML, c=1 | JSON, c=8 | XML, c=8 |
| --- | ---: | ---: | ---: | ---: |
| fake-gcs-server | 415 | 437 | 2,141 | 2,271 |
| 8 workers | 377 | 364 | 2,269 | 2,154 |
| 8 workers, a second container | 410 | 370 | 2,335 | 2,236 |
| 2 workers | 413 | 409 | 861 | 859 |
| 1 worker | 518 | 469 | 439 | 481 |

On a single connection one worker was the fastest configuration. `strace -c` over 3 seconds of
JSON downloads at one connection showed why:

| Process | Body reads | Body writes | Other |
| --- | --- | --- | --- |
| fake-gcs-server | 23,450 `read`, about 243 KiB each on average | 23,237 `write` | 10,614 `nanosleep` |
| This change, 8 workers | 6,070 `preadv2` at 184 µs | 6,101 `writev` at 119 µs | 1,000 `futex`, about two per request |
| This change, 1 worker | 9,922 `preadv2` at 119 µs | 9,923 `writev` at 84 µs | no `futex` |

With eight workers there were about two `futex` calls per request, and the same copies took about
1.5 times as long. That fits Tokio waking another worker per request and the connection's task
moving to a vCPU that had been idle at a lower clock, but neither the move nor the clock was
verified. fake-gcs-server copies with plain
`read` and `write` rather than `sendfile`, so both servers copy the bytes twice. `xfs_io pread`
read a cached 1 GiB file at 33 to 36 GiB/s with 64 KiB, 256 KiB, 1 MiB and 4 MiB reads, so the
read size does not limit either one.

## Allocator and huge pages

Two comparison builds differ from this change only by one line each: one keeps musl's allocator
and the other keeps transparent huge pages. Each cell gives requests per second for 100 KiB
objects.

| Build | Metadata, c=1 / 8 / 32 / 128 | Media (JSON), c=1 / 8 / 32 / 128 |
| --- | --- | --- |
| fake-gcs-server | 9,135 / 37,467 / 96,169 / 114,497 | 7,536 / 32,666 / 57,374 / 59,895 |
| This change | 15,054 / 65,832 / 155,200 / 188,599 | 9,286 / 51,070 / 111,280 / 125,267 |
| musl's allocator | 11,843 / 11,572 / 11,860 / 11,711 | 5,813 / 10,266 / 11,506 / 11,494 |
| Huge pages kept | 15,049 / 66,186 / 159,297 / 191,788 | 9,245 / 50,649 / 105,611 / 125,908 |

With musl's allocator the rate stays near 11,700 requests per second at every concurrency.
Keeping huge pages changes no rate, only memory.

## Memory

Process RSS and container memory after seeding 1,000 objects of 100 KiB and loading them at 32
connections, in MiB. Five seconds later, no value had changed.

| Build | Idle | Seeded | After the load |
| --- | --- | --- | --- |
| fake-gcs-server | 33.5 / 13.5 | 40.3 / 25.8 | 60.5 / 59.9 |
| 0.12.0 | 7.3 / 2.1 | 10.3 / 8.4 | 10.5 / 8.5 |
| This change | 7.9 / 3.1 | 18.9 / 17.4 | 31.8 / 30.5 |
| musl's allocator | 7.1 / 2.3 | 10.8 / 9.3 | 20.0 / 18.6 |
| Huge pages kept | 15.2 / 10.5 | 52.2 / 51.2 | 77.9 / 103.9 |

This change holds about 21 MiB more than 0.12.0 after the load. About 12 MiB of that is the
allocator, which keeps freed pages. The rest is the reader threads with their SQLite caches, the
cached records and the pooled buffers. Memory still does not grow with object size: the cache
holds at most 4,096 records and no object bytes.

After 100 objects of 10 MiB were loaded at 128 connections, anonymous memory was 63.3 MiB for this
change and 46.6 MiB for fake-gcs-server. Container memory was 101.0 and 103.8 MiB. In earlier runs
of the build before the review fixes, anonymous memory after the same load was 55.4 MiB once and
76.2 MiB once. Allowing at most sixteen 1 MiB buffers at once was tried and left 80.6 MiB, so that
bound was not kept.

Container memory after each matrix above, in MiB:

| Objects | fake-gcs-server | 0.12.0 | This change | Same build again |
| --- | ---: | ---: | ---: | ---: |
| 1 KiB | 47.7 | 10.1 | 29.9 | |
| 100 KiB | 71.2 | 8.4 | 29.7 | |
| 1 MiB | 78.0 | 38.3 | 82.8 | 74.3 |
| 10 MiB | 92.8 | 37.8 | 77.9 | 129.7 |

## Image size

`docker image inspect` sizes for linux/arm64, in bytes:

| Image | Bytes |
| --- | ---: |
| fake-gcs-server 1.56.1 | 80,871,020 |
| gcs-mimic 0.12.0 | 25,931,247 |
| This change | 26,371,602 |
| This change with musl's allocator | 26,254,252 |

## Limitations

- One host. Docker Desktop on Apple silicon has performance and efficiency cores, and the
  single-connection gap depends on how the host schedules idle vCPUs. Linux hosts were not
  measured.
- Any catalog write clears the whole read cache, and the writer thread closes the cached files,
  up to 1,024 of them. A server that writes steadily, such as one delivering notifications, gains
  little from the cache.
- Downloads on macOS and other systems without `RWF_NOWAIT` read on the blocking pool, as before.
  The pooled reader still reads the body's start there.
- At measurement time, the load generator and its orchestration were external.
  The [original client source](read-load-client.go.txt) is now archived as a
  measurement input; the original orchestration scripts are not included.

## Checks

Recorded with the commands' own output for this source:

| Command | Result |
| --- | --- |
| `cargo test --workspace --no-fail-fast` | PASS: 2,944 passed, 0 failed, 1 ignored (the opt-in Pub/Sub emulator test) |
| `cargo xtask lint` | PASS: 0 new production violations, 0 ratcheted findings |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `bash scripts/check-cross.sh` | PASS: Clippy and `cargo xtask lint --target` for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl` |
| `bash scripts/check-docs.sh` | PASS: 606 feature rows validated, release note and Docker Hub overview checks |
| `cargo fmt --all --check` | PASS |
| Read path unit tests in a `rust:1.99.1-slim-trixie` container (`cargo test --lib --locked` filtered to the reader, read cache, read pool, object body, gzip and HTTP tests) | PASS: 180 passed, including `bytes_in_the_page_cache_read_without_a_blocking_call`, which needs Linux |
| `just check-container linux/arm64` and `just check-container linux/amd64` (emulated) | PASS on both: all five smoke sections, from isolated health and media CRUD through persistence, seeding and `sync_dir` export |
| `cargo xtask dependencies check` | `mimalloc` 0.1.52 `CURRENT`. Exit 1 only because `com.google.cloud:google-cloud-storage` 2.78.0 replaced the 2.77.0 that the Java SDK suites pin; that update belongs to a dependency refresh |
