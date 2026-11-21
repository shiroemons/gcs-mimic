# Synchronization bind-mount stale reads: evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [native write-back recovery](../slices/native-writeback-recovery.md)
and [directory synchronization](../slices/directory-synchronization.md). This
record supports the rule that a tracked body is judged by its size, MD5 and
CRC32C and never by its modification time: a container that reads a host
file's old bytes through a bind mount also reads its old size and
modification time, so comparing the time would protect nothing.

Recorded on **2026-11-21** with Docker Desktop **29.9.3** on a macOS
**27.2.1** arm64 host, running a `debian:trixie-slim` linux/arm64 container
that bind-mounts a host directory. The host directory was on an exFAT volume
in one series and on APFS in the other. The host rewrote one 4 KiB file
100 to 180 times per condition, at random intervals of 0 to 200 ms, while the
container observed it with `stat`, an open and a hash of its bytes.

## Results

| Condition | exFAT | APFS |
| --- | --- | --- |
| In-place overwrite that restores the old size and modification time (`touch -r` and `dd conv=notrunc`) | The container's hash showed the new bytes every time; 0 reads kept the old bytes, and the new bytes were visible within 0.30 s at most | Same as exFAT |
| Old bytes seen after a replacement | Only right after a `mv` replacement: 10 times, each within 6.9 ms, and each time the container's size and modification time were the old ones too | 0 times |
| `stat` succeeds and the following open fails with `ENOENT` | After creation, overwrite and `mv`, for up to 238 ms (while the file was being replaced repeatedly) | Only after `mv`, for up to 132 ms |
| `stat` (`fstatat`) fails with `ENOENT` | 0 times in more than 40,000 observations | 0 times in more than 40,000 observations |
| A directory transiently missing (`ENOENT`) | 0 times | 0 times |

Whenever the container read old bytes, the size and modification time it read
were the old ones too. An overwrite that put back the old size and
modification time still showed its new bytes to the container's hash.

## Limits

- One container, one 4 KiB file and the trial counts above. Containers of
  other projects were running at the same time.
- While the host runs `mkdir`, the container's `stat` and `ls` wait for it, so
  a window in which a directory is missing may be hidden rather than absent.
- The measurement covers Docker Desktop's file sharing on this host only; it
  says nothing about other container runtimes, network filesystems or Linux
  hosts.
