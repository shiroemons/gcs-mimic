# Comparison of gcs-mimic 0.14.0 with other emulators

<!-- markdownlint-configure-file {"MD013": false} -->

Measured on 2026-11-08 (UTC). These are the numbers behind the README's "By the numbers" table: observations from one host, not a gate and not a general ranking. Raw values are in [product-comparison-0.14.0.tsv](product-comparison-0.14.0.tsv).

## Inputs

| Input | Identity |
| --- | --- |
| gcs-mimic 0.14.0 | `ghcr.io/shiroemons/gcs-mimic@sha256:f45b1324f63aff31575e0836f1892424ce360af6c081148b0407aa2dfa43eb4f` |
| fake-gcs-server 1.56.1 | `fsouza/fake-gcs-server@sha256:797ce226d62f947c009dc40246b30cfb456b8473d8241407f9d6f2c04e4d69ef` |
| gcp-storage-emulator v2026.07.19 | `oittaa/gcp-storage-emulator@sha256:d5652358036510c4a7395ea9dc2487cf3515b583065b2ba4086a398e41768ba8` |
| Load client | [read-load-client.go.txt](read-load-client.go.txt), SHA-256 `9c36385be49534451fdf62c3d71bbc39dbc71e38efe5370ca0cbc0e79cabbf73`, built with Go 1.24.5 for linux/arm64 (binary SHA-256 `0a9259df22dba2b17db355c95dda791fefc6f093df2f1c78b014220644a8c652`) |
| Host | macOS on Apple silicon; Docker Desktop VM with 14 vCPUs, 15.6 GiB, kernel `7.0.14-linuxkit`, linux/arm64 |

Fourteen containers of other projects kept running. The host load average was between 3 and 11 during the runs.

## Method

- Each server ran alone with default settings, plain HTTP and a named data volume: gcs-mimic with `GCS_MIMIC_SCHEME=http` (permissive), fake-gcs-server with `-scheme http -backend filesystem` and a matching `-public-host`, and gcp-storage-emulator with `start` on disk.
- Each object size had its own freshly started server: 1,000 objects of 1 KiB, 100 KiB or 1 MiB, or 100 objects of 10 MiB. The server was then seeded, warmed for 2 seconds with 8 connections and measured.
- The load client ran in another container on the same Docker network. It sent closed-loop GETs for 3 seconds per cell over HTTP/1.1 keep-alive and read every body. Every response was checked for status 200 and the expected length.
- There were three rounds, and the order of the servers rotated between them. Tables give the median of the three rounds.
- Uploads are the seeding step: single-request media uploads over 8 connections. There is one sample per round.
- Memory comes from a separate pass on fresh servers. It is `VmRSS` of PID 1 when idle and after seeding 1,000 objects of 100 KiB and reading them with 32 connections.
- Every measured cell reported zero errors. gcp-storage-emulator was measured up to 32 connections, because at 128 connections an earlier run returned client errors.

## Downloads

Requests per second, median of three rounds. `JSON` is `?alt=media` and `XML` is the path-style `/{bucket}/{object}`.

| Object | API | Connections | gcs-mimic | fake-gcs-server | gcp-storage-emulator | gcs-mimic / fake-gcs-server |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | JSON | 1 | 14,562 | 9,226 | 2,939 | 1.58 |
| 1 KiB | JSON | 8 | 63,956 | 39,530 | 6,748 | 1.62 |
| 1 KiB | JSON | 32 | 157,903 | 87,052 | 6,820 | 1.81 |
| 1 KiB | JSON | 128 | 193,825 | 97,126 | not measured | 2.00 |
| 1 KiB | XML | 1 | 14,781 | 9,663 | 2,970 | 1.53 |
| 1 KiB | XML | 8 | 63,747 | 41,428 | 8,024 | 1.54 |
| 1 KiB | XML | 32 | 163,870 | 96,145 | 6,863 | 1.70 |
| 1 KiB | XML | 128 | 193,521 | 109,865 | not measured | 1.76 |
| 100 KiB | JSON | 1 | 9,444 | 7,460 | 2,678 | 1.27 |
| 100 KiB | JSON | 8 | 51,892 | 31,446 | 5,574 | 1.65 |
| 100 KiB | JSON | 32 | 115,996 | 54,991 | 3,475 | 2.11 |
| 100 KiB | JSON | 128 | 129,222 | 59,308 | not measured | 2.18 |
| 100 KiB | XML | 1 | 9,616 | 8,020 | 2,664 | 1.20 |
| 100 KiB | XML | 8 | 52,285 | 32,832 | 5,859 | 1.59 |
| 100 KiB | XML | 32 | 114,939 | 59,302 | 3,351 | 1.94 |
| 100 KiB | XML | 128 | 128,596 | 63,439 | not measured | 2.03 |
| 1 MiB | JSON | 1 | 3,495 | 3,005 | 1,689 | 1.16 |
| 1 MiB | JSON | 8 | 18,753 | 15,139 | 2,811 | 1.24 |
| 1 MiB | JSON | 32 | 27,063 | 21,520 | 2,045 | 1.26 |
| 1 MiB | JSON | 128 | 26,543 | 21,720 | not measured | 1.22 |
| 1 MiB | XML | 1 | 3,467 | 3,165 | 1,676 | 1.10 |
| 1 MiB | XML | 8 | 19,074 | 15,070 | 2,821 | 1.27 |
| 1 MiB | XML | 32 | 27,050 | 21,458 | 2,229 | 1.26 |
| 1 MiB | XML | 128 | 26,594 | 21,433 | not measured | 1.24 |
| 10 MiB | JSON | 1 | 469 | 479 | 376 | 0.98 |
| 10 MiB | JSON | 8 | 2,665 | 2,525 | 422 | 1.06 |
| 10 MiB | JSON | 32 | 2,890 | 2,938 | 323 | 0.98 |
| 10 MiB | JSON | 128 | 2,816 | 2,968 | not measured | 0.95 |
| 10 MiB | XML | 1 | 467 | 478 | 378 | 0.98 |
| 10 MiB | XML | 8 | 2,604 | 2,503 | 395 | 1.04 |
| 10 MiB | XML | 32 | 2,870 | 2,992 | 203 | 0.96 |
| 10 MiB | XML | 128 | 2,788 | 3,109 | not measured | 0.90 |

## Uploads

Seconds to upload the whole set over 8 connections, median of three rounds. Lower is better.

| Objects | gcs-mimic | fake-gcs-server | gcp-storage-emulator | fake-gcs-server / gcs-mimic |
| --- | ---: | ---: | ---: | ---: |
| 1,000 of 1 KiB | 0.441 | 0.826 | 2.065 | 1.87 |
| 1,000 of 100 KiB | 0.629 | 1.290 | 2.091 | 2.05 |
| 1,000 of 1 MiB | 1.344 | 3.153 | 3.108 | 2.35 |
| 100 of 10 MiB | 1.071 | 2.235 | 1.670 | 2.09 |

Faster uploads do not imply equal durability. The products make different crash and power-loss guarantees.

## Memory

Process RSS in MiB.

| State | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | ---: | ---: | ---: |
| Idle | 9.7 | 34.0 | 27.3 |
| After seeding 1,000 objects of 100 KiB | 19.0 | 39.0 | 32.4 |
| After reading them with 32 connections | 29.8 | 54.2 | 32.4 |

Memory after sustained reads of large objects was not measured here.

## Limits

- One host and one run of three rounds. This is Docker Desktop on Apple silicon, not a native Linux host, with other projects' containers running.
- Plain HTTP and each server's default profile. TLS, strict authentication, listing, mixed reads and writes, range and conditional requests were not measured.
- Each server held one object size. In permissive mode, gcs-mimic answers repeated reads from a cache that holds about 1,000 open files and admits nothing new until the next catalog write (`src/storage/catalog/read_cache.rs`). Sized that way, the measured working set fits.
- A working set larger than the cache takes the slower pooled path. In a separate run with 3,100 objects on one server and no write between sizes, buckets read after the cache filled were served at about 7,100 to 8,400 requests per second at 8 or more connections for 100 KiB and 1 MiB objects, while fake-gcs-server served 31,000 to 62,000 and 15,000 to 22,000.
- An earlier attempt was stopped because the measurement filled the Docker VM's disk. Its samples are not used here.
