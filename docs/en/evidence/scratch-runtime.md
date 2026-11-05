# Scratch runtime verification

Verified on 2026-11-05 with the accompanying Dockerfile, xtask and dependency
snapshot changes. Runtime application code is unchanged.

## Scope and acceptance

The container moves from a GNU/glibc binary on digest-pinned
`gcr.io/distroless/cc-debian13:nonroot` to a statically linked musl binary on
`scratch`. The build stage installs `musl-tools` from the Debian snapshot the
pinned Rust image was built from and compiles with fat LTO and
`opt-level = "s"`; the release archives keep the default release profile.
Numeric UID/GID 10001, the read-only root, the probes and every checksum,
authorization, synchronization and durability setting are unchanged.

The image ships the binary, its notices under `/usr/share/licenses/gcs-mimic`,
the UID/GID 10001 data directory and a mode-1777 `/tmp` for import spooling.
The binary links only libc, TLS uses rustls with an embedded trust store, and no
code reads `/etc/ssl`, time zone data, `/etc/passwd` or a home directory, so the
image carries none of them. It contains no shell or package manager.

Acceptance requires both Linux architectures to build and pass every existing
container smoke case, and the release notices to cover the musl runtime.

## Inputs

- Rust 1.99.1 from `rust:1.99.1-slim-trixie` index
  `sha256:ea0999ff67355d958b8799b6751c879a6d770076d26047dca66f6e51e77930d3`.
  Its linux/amd64 and linux/arm64 manifests are identical to those of the
  later index adopted in the dependency refresh, which changed only linux/riscv64.
- `musl-tools`, `musl-dev` and `musl` `1.2.5-3.1~deb13u1` from
  `snapshot.debian.org` timestamp `20261027T000000Z`.
- The committed Cargo.lock and the pinned musl 1.2.5 source notice archive.

The host is macOS arm64: linux/arm64 runs natively and linux/amd64 uses
emulation. Remote GitHub Actions execution was not part of this verification.

## Size measurements

Uncompressed bytes are `docker image inspect` sizes. Compressed bytes are the
sum of the platform manifest's gzip layer sizes: registry manifests for the
published images, and the layer blobs of `docker image save` for the local
builds.

| Image | Platform | Uncompressed bytes | Compressed bytes |
| --- | --- | --- | --- |
| gcs-mimic 0.10.0 (Distroless cc) | linux/amd64 | 92,830,510 | 20,446,571 |
| gcs-mimic 0.10.0 (Distroless cc) | linux/arm64 | 100,749,862 | 20,579,427 |
| gcs-mimic scratch | linux/amd64 | 28,041,750 | 8,110,172 |
| gcs-mimic scratch | linux/arm64 | 26,042,517 | 7,900,891 |
| fake-gcs-server 1.56.1 | linux/amd64 | 84,325,708 | 25,430,173 |
| fake-gcs-server 1.56.1 | linux/arm64 | 80,871,020 | 24,179,132 |

The scratch image is about 70% smaller uncompressed and 60% smaller compressed
than 0.10.0 on each architecture. The base image accounted for about 43 MB of
0.10.0's arm64 size, and the toolchain-wide Rust `COPYRIGHT.html` notice for
14.4 MB; the release notices now carry only the standard library's
`COPYRIGHT-library.html`.

| linux/arm64 musl binary, stripped | Bytes |
| --- | --- |
| Thin LTO, default `opt-level` | 18,181,264 |
| Fat LTO, default `opt-level` | 17,256,720 |
| Fat LTO, `opt-level = "s"` (adopted) | 12,538,248 |

The adopted linux/amd64 binary is 14,328,928 bytes.

## Throughput

One short local run on linux/arm64, each container limited to 2 CPUs and
256 MiB, over HTTP with ApacheBench without keep-alive: 5,000 GETs of a 17-byte
object at concurrency 32, 200 GETs of an 8 MiB object at concurrency 8, and the
summed time of 40 sequential 8 MiB media uploads. These are single
observations, not a gate.

| Build | Small GET requests/s | 8 MiB GET requests/s | 40 uploads |
| --- | --- | --- | --- |
| glibc, Distroless cc, same source | 7,656 / 9,028 | 36.8 / 37.3 | 1.65 s / 1.54 s |
| musl, default profile | 6,375 / 5,506 | 36.2 / 36.3 | 1.85 s / 1.72 s |
| musl, fat LTO and `opt-level = "s"` (adopted) | 6,024 / 5,895 | 37.1 / 36.4 | 1.54 s / 1.54 s |

Large transfers are unchanged. Small requests are slower on musl because of its
allocator; a trial build with mimalloc restored glibc's small-request rate but
doubled resident memory (about 32 MiB against 11 MiB), so no allocator
dependency is added. In an earlier run of the same workload, `opt-level = "s"`
with thin LTO was 5-10% slower than the default musl profile, which is why the
container build uses fat LTO.

## Checks

- `just check-containers` built both architectures from this source. linux/arm64
  passed all five smoke sections: isolated health, host/DNS access, media CRUD,
  persistence, volume reset, resource limits and signals; the bucket
  initialization mount; seed replay; the implicit `/data` seed; and `sync_dir`
  export. The linux/amd64 build first failed when `snapshot.debian.org`
  answered 504 and 503; `scripts/install-musl-tools.sh` then raised apt's
  retries from three to eight, and `just check-container linux/amd64` passed
  the same five sections. The arm64 run used the earlier retry count, and
  nothing else differs.
- Both images report `gcs-mimic 0.10.0`, run as `10001:10001`, contain no
  `bin/sh` or `usr/bin/sh` and no `rust/COPYRIGHT.html`, and include
  `native/musl-1.2.5.tar.gz` and `native/GCC-COPYRIGHT`.
- `cargo xtask dependencies check --scope release` compared the musl package
  (`CURRENT`) and the Debian stable codename (`CURRENT`) and skipped the
  development-only socat package. It exited 1 only because the Rust index had
  been rebuilt upstream; that update is adopted separately.
