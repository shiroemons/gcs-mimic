# Large uploads under a container memory limit

Measured on 2026-11-10 (UTC). These are observations from one host, not a gate. They record why a 128 MiB container could be OOM-killed while receiving a 1 GiB object, and what changed.

## Setup

- Host: macOS on Apple silicon; Docker Desktop VM with 14 vCPUs, 15.6 GiB, kernel `7.0.14-linuxkit`, cgroup v2, linux/arm64. Containers of other projects kept running, and the host load average was 9 to 21.
- Servers: `ghcr.io/shiroemons/gcs-mimic:0.16.0`, `ghcr.io/shiroemons/gcs-mimic:0.17.0` and the repository `Dockerfile` build of this change, each started with `--scheme http`, `--memory=128m --memory-swap=128m` and no volume, so the data directory was on the container's overlayfs layer.
- One transfer is an upload of a 1 GiB object of random bytes with curl, then a download whose SHA-256 is compared with the source. It counts as passed when the upload returned 200 and the bytes matched. A round is a fresh container with one single-request (`uploadType=media`) transfer and one resumable transfer sent as a single chunk.
- Process memory is `VmRSS` of PID 1, sampled five times per second from a sidecar in the server's PID namespace.

## What failed

Releases up to 0.17.0 wrote a staged body through the page cache and synced it once, at the end. The kernel report of one kill on 0.17.0:

```text
tokio-rt-worker invoked oom-killer: gfp_mask=0x101cca(GFP_HIGHUSER_MOVABLE|__GFP_WRITE), order=2
 mem_cgroup_out_of_memory
 try_charge_memcg
 filemap_add_folio
 ext4_da_write_begin
 ext4_buffered_write_iter
 backing_file_write_iter
 ovl_write_iter
 vfs_write
memory: usage 131548kB, limit 131072kB, failcnt 10334
anon 7868416
file 118980608
file_dirty 108134400
file_writeback 10846208
Memory cgroup out of memory: Killed process 9981 (gcs-mimic) total-vm:1104940kB, anon-rss:7684kB, file-rss:8452kB
```

Of the 125 MiB charged to the container, 103 MiB were dirty pages and 10 MiB were under writeback, while the process held 7.5 MiB of anonymous memory. Dirty pages cannot be reclaimed before they are written, so the write that needed one more page had nothing to free. The write went through overlayfs, where the kernel did not hold the writer back to the container's limit.

The failure depended on how busy the disk was. On the same day, 0.17.0 completed 40 uploads in a row without a kill at one time and lost 11 of 60 transfers at another.

## What changed

On Linux a staged body is now synced after every 8 MiB written. The final sync that decides durability is unchanged.

Peak of the container's `memory.stat` over one round without a kill, sampled ten times per second:

| | 0.17.0 | This change |
| --- | ---: | ---: |
| `file_dirty` | 37.6 MiB | 8.0 MiB |
| `file_writeback` | 41.5 MiB | 7.8 MiB |

Transfers that passed, of those attempted:

| Run | 0.16.0 | 0.17.0 | This change |
| --- | ---: | ---: | ---: |
| 0.17.0 alone, 30 rounds | | 49 of 60 | |
| 0.16.0 and 0.17.0 taking turns, 3 × 5 rounds each | 23 of 30 | 28 of 30 | |
| 0.17.0 and this change taking turns, 3 × 5 rounds each | | 26 of 30 | 30 of 30 |
| This change alone, 30 rounds | | | 60 of 60 |
| This change alone, 5 containers with 10 transfers each | | | 50 of 50 |
| Total | 23 of 30 | 103 of 120 | 140 of 140 |

Every failed transfer was a container killed by the kernel with `OOMKilled=true`. The largest process RSS sample was 16.0 MiB on 0.17.0 and 16.5 MiB with this change.

With a 256 MiB limit, 0.17.0 passed 40 of 40 transfers in the same session.

Upload time on containers without a memory limit, three alternating passes:

| | 0.17.0 | This change |
| --- | ---: | ---: |
| One 1 GiB single-request upload | 5.04 to 5.54 s | 4.59 to 4.97 s |
| 200 single-request uploads of 1 MiB | 1.76 to 2.06 s | 1.53 to 1.89 s |

## Limitations

- One host and one filesystem stack. A data directory on a named volume, a bind mount or a Linux host was not measured.
- 140 transfers without a kill do not prove that none can happen. The measured cause is removed: unsynced bytes of one body stay within 8 MiB. Each concurrent upload has its own 8 MiB, and other writers were not measured.
- Pages already written stay in the page cache until the kernel reclaims them, so the container's charged memory still reaches its limit during a large transfer.
- The scripts that ran these transfers are not part of the repository.
