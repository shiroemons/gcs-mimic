# Comparison of gcs-mimic 0.16.0 with other emulators

<!-- markdownlint-configure-file {"MD013": false} -->

Measured on 2026-11-09 (UTC). These are the numbers behind the README's "By the numbers" table: observations from one host, not a gate and not a general ranking. Raw values are in [product-comparison-0.16.0.tsv](product-comparison-0.16.0.tsv). The method repeats the [0.14.0 comparison](product-comparison-0.14.0.md) and adds floci-gcp. Compare numbers within one run only: the host differed between runs.

## Inputs

| Input | Identity |
| --- | --- |
| gcs-mimic 0.16.0 | `ghcr.io/shiroemons/gcs-mimic@sha256:ff15b8064c1cef73c2f4578d3b84e6b219e17729c3227e6577879c52e8562bc4` |
| fake-gcs-server 1.56.1 | `fsouza/fake-gcs-server@sha256:797ce226d62f947c009dc40246b30cfb456b8473d8241407f9d6f2c04e4d69ef` |
| gcp-storage-emulator v2026.07.19 | `oittaa/gcp-storage-emulator@sha256:d5652358036510c4a7395ea9dc2487cf3515b583065b2ba4086a398e41768ba8` |
| floci-gcp 0.12.0 (native image, MIT) | `floci/floci-gcp@sha256:d02e70407d6b37c3e24e31b5ccf100606735cba4f84fd476708b35afd052abb3` |
| Load client | [read-load-client.go.txt](read-load-client.go.txt), SHA-256 `9c36385be49534451fdf62c3d71bbc39dbc71e38efe5370ca0cbc0e79cabbf73`, built with Go 1.24.5 for linux/arm64 (binary SHA-256 `7a20eee1ea3296aca2305b7f5820eb10584db0814aa1758db7adc9ac44176d3b`) |
| Host | macOS on Apple silicon; Docker Desktop VM with 14 vCPUs, 15.6 GiB, kernel `7.0.14-linuxkit`, linux/arm64 |

Fourteen containers of other projects kept running. The host load average was about 4 when the run started, and light read-only commands ran beside it.

## Compressed image size

Sum of the layer sizes of the `linux/amd64` manifest.

| gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| ---: | ---: | ---: | ---: |
| 8.4 MB | 25.4 MB | 48.8 MB | 67.2 MB |

## Method

- Each server ran alone on plain HTTP with a named data volume: gcs-mimic with `GCS_MIMIC_SCHEME=http` (permissive), fake-gcs-server with `-scheme http -backend filesystem` and a matching `-public-host`, gcp-storage-emulator with `start` on disk, and floci-gcp with `FLOCI_GCP_STORAGE_MODE=hybrid`.
- floci-gcp's default storage mode is `memory`, which keeps nothing across a restart. `hybrid` is the mode its quick start and documentation use for local development, and the one comparable to the other servers' on-disk storage.
- Each object size had its own freshly started server: 1,000 objects of 1 KiB, 100 KiB or 1 MiB, or 100 objects of 10 MiB. The server was then seeded, warmed for 2 seconds with 8 connections and measured.
- The load client ran in another container on the same Docker network. It sent closed-loop GETs for 3 seconds per cell over HTTP/1.1 keep-alive and read every body. Every response was checked for status 200 and the expected length.
- There were three rounds, and the order of the servers rotated between them. Tables give the median of the three rounds.
- Uploads are the seeding step: single-request media uploads over 8 connections. There is one sample per round.
- Memory comes from a separate pass on fresh servers. It is `VmRSS` of PID 1 when idle, 6 seconds after seeding 1,000 objects of 100 KiB, and after reading them for 10 seconds with 32 connections. The cgroup figure from `docker stats` was recorded at each point.
- Every measured cell reported zero errors. gcp-storage-emulator was measured up to 32 connections, as in the earlier comparison.

## Downloads

Requests per second, median of three rounds. `JSON` is `?alt=media` and `XML` is the path-style `/{bucket}/{object}`. The last two columns divide by fake-gcs-server.

| Object | API | Connections | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp | gcs-mimic / fake-gcs-server | floci-gcp / fake-gcs-server |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | JSON | 1 | 13,915 | 6,236 | 1,691 | 4,337 | 2.23 | 0.70 |
| 1 KiB | JSON | 8 | 60,889 | 27,541 | 5,699 | 22,674 | 2.21 | 0.82 |
| 1 KiB | JSON | 32 | 118,242 | 51,765 | 4,107 | 38,720 | 2.28 | 0.75 |
| 1 KiB | JSON | 128 | 159,919 | 63,514 | not measured | 59,358 | 2.52 | 0.93 |
| 1 KiB | XML | 1 | 14,507 | 6,497 | 1,527 | 5,592 | 2.23 | 0.86 |
| 1 KiB | XML | 8 | 59,175 | 30,901 | 5,214 | 33,928 | 1.91 | 1.10 |
| 1 KiB | XML | 32 | 114,986 | 61,989 | 5,041 | 65,179 | 1.85 | 1.05 |
| 1 KiB | XML | 128 | 158,456 | 58,868 | not measured | 75,281 | 2.69 | 1.28 |
| 100 KiB | JSON | 1 | 8,325 | 5,891 | 1,686 | 3,677 | 1.41 | 0.62 |
| 100 KiB | JSON | 8 | 42,143 | 23,596 | 5,568 | 28,551 | 1.79 | 1.21 |
| 100 KiB | JSON | 32 | 79,500 | 40,743 | 3,733 | 50,233 | 1.95 | 1.23 |
| 100 KiB | JSON | 128 | 114,314 | 44,356 | not measured | 65,495 | 2.58 | 1.48 |
| 100 KiB | XML | 1 | 8,142 | 6,606 | 1,631 | 5,215 | 1.23 | 0.79 |
| 100 KiB | XML | 8 | 38,570 | 27,802 | 4,839 | 29,809 | 1.39 | 1.07 |
| 100 KiB | XML | 32 | 82,885 | 50,379 | 4,051 | 48,947 | 1.65 | 0.97 |
| 100 KiB | XML | 128 | 91,027 | 56,114 | not measured | 55,474 | 1.62 | 0.99 |
| 1 MiB | JSON | 1 | 3,030 | 2,428 | 1,372 | 2,452 | 1.25 | 1.01 |
| 1 MiB | JSON | 8 | 15,516 | 11,923 | 2,133 | 12,265 | 1.30 | 1.03 |
| 1 MiB | JSON | 32 | 18,218 | 15,822 | 1,791 | 14,911 | 1.15 | 0.94 |
| 1 MiB | JSON | 128 | 18,528 | 17,804 | not measured | 15,523 | 1.04 | 0.87 |
| 1 MiB | XML | 1 | 2,636 | 2,653 | 1,227 | 2,439 | 0.99 | 0.92 |
| 1 MiB | XML | 8 | 14,429 | 13,399 | 1,785 | 14,147 | 1.08 | 1.06 |
| 1 MiB | XML | 32 | 20,424 | 16,523 | 1,446 | 15,611 | 1.24 | 0.94 |
| 1 MiB | XML | 128 | 22,113 | 19,081 | not measured | 16,530 | 1.16 | 0.87 |
| 10 MiB | JSON | 1 | 333 | 413 | 258 | 383 | 0.81 | 0.93 |
| 10 MiB | JSON | 8 | 2,080 | 2,178 | 321 | 1,898 | 0.96 | 0.87 |
| 10 MiB | JSON | 32 | 1,893 | 2,190 | 228 | 1,919 | 0.86 | 0.88 |
| 10 MiB | JSON | 128 | 1,966 | 2,696 | not measured | 1,623 | 0.73 | 0.60 |
| 10 MiB | XML | 1 | 366 | 441 | 291 | 414 | 0.83 | 0.94 |
| 10 MiB | XML | 8 | 2,159 | 1,876 | 342 | 1,870 | 1.15 | 1.00 |
| 10 MiB | XML | 32 | 2,254 | 2,440 | 252 | 1,908 | 0.92 | 0.78 |
| 10 MiB | XML | 128 | 2,087 | 2,590 | not measured | 1,236 | 0.81 | 0.48 |

## Uploads

Seconds to upload the whole set over 8 connections, median of three rounds. Lower is better. The last columns divide fake-gcs-server's time by the other product's.

| Objects | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp | fake-gcs-server / gcs-mimic | fake-gcs-server / floci-gcp |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 of 1 KiB | 0.736 | 2.609 | 2.062 | 0.106 | 3.54 | 24.6 |
| 1,000 of 100 KiB | 1.859 | 2.017 | 2.081 | 0.368 | 1.08 | 5.48 |
| 1,000 of 1 MiB | 1.835 | 5.689 | 3.737 | 2.284 | 3.10 | 2.49 |
| 100 of 10 MiB | 1.328 | 2.629 | 2.069 | 2.265 | 1.98 | 1.16 |

Faster uploads do not imply equal durability. The products make different crash and power-loss guarantees. floci-gcp's `hybrid` mode answers from memory and flushes to disk every 5 seconds, so an upload does not wait for the disk, while gcs-mimic makes an object durable before it answers.

## Memory

Process RSS in MiB.

| State | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | ---: | ---: | ---: | ---: |
| Idle | 9.9 | 33.9 | 27.3 | 103.6 |
| After seeding 1,000 objects of 100 KiB | 19.5 | 40.3 | 32.0 | 278.1 |
| After reading them with 32 connections | 29.3 | 57.2 | 32.0 | 320.0 |

Cgroup memory from `docker stats` in MiB, which leaves out reclaimable file pages.

| State | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | ---: | ---: | ---: | ---: |
| Idle | 4.8 | 13.2 | 15.5 | 18.8 |
| After seeding 1,000 objects of 100 KiB | 18.9 | 25.7 | 24.8 | 193.2 |
| After reading them with 32 connections | 29.1 | 59.9 | 25.1 | 234.9 |

RSS counts the mapped pages of a program's own binary, and floci-gcp's native binary is 174 MiB, so its idle RSS is much larger than its idle cgroup figure. Its memory grows with the stored objects because `hybrid` keeps them in memory. Memory after sustained reads of large objects was not measured here.

## Limits

- One host and one run of three rounds. This is Docker Desktop on Apple silicon, not a native Linux host, with other projects' containers running.
- Plain HTTP. TLS, strict authentication, listing, mixed reads and writes, range and conditional requests were not measured.
- floci-gcp emulates more than two dozen other Google Cloud services in the same process and image, which the Cloud Storage figures here do not show. It logs each request at `INFO` by default, and that setting was left as it is.
- Each server held one object size. The notes on gcs-mimic's read cache and the separate run with 3,100 objects in the [0.14.0 comparison](product-comparison-0.14.0.md#limits) were not repeated for this release.
