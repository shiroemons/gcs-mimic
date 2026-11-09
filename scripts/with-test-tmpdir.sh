#!/usr/bin/env bash
# Runs a command, optionally with TMPDIR on a temporary macOS RAM disk. Tests keep their real sync
# calls, but macOS implements them with F_FULLFSYNC, which flushes the whole system disk and
# dominates test time. On a RAM disk the same calls finish quickly, so the wrapper also raises the
# default test binary concurrency. The RAM disk is opt-in (GCSM_TEST_RAMDISK=1): it pins memory,
# and a disk that cannot be detached keeps that memory until reboot or a manual detach, which can
# destabilize the whole machine, while the default only costs test time. The default also matches
# CI timing. Other platforms run the command unchanged with serial test binaries. The GCSM_TEST_
# settings stay outside the GCS_MIMIC_ prefix, which the server reads as its own settings and
# rejects when unknown, so exporting them in a shell profile cannot stop a server
# from starting.
set -euo pipefail

if [[ $# == 0 ]]; then
  echo 'Usage: bash scripts/with-test-tmpdir.sh COMMAND [ARGUMENT...]' >&2
  exit 2
fi
case ${GCSM_TEST_RAMDISK:-0} in
  0) use_ramdisk=false ;;
  1) use_ramdisk=true ;;
  *) echo 'GCSM_TEST_RAMDISK must be 0 or 1' >&2; exit 2 ;;
esac
if [[ $(uname -s) != Darwin ]]; then exec "$@"; fi
size_mb=${GCSM_TEST_RAMDISK_MB:-4096}
if $use_ramdisk && [[ ! $size_mb =~ ^[1-9][0-9]*$ ]]; then
  echo 'GCSM_TEST_RAMDISK_MB must be a positive integer' >&2
  exit 2
fi

volume_prefix=gcs-mimic-tmp-

# A process that outlived its test run (a leaked server, or a build daemon that inherited TMPDIR)
# keeps the volume busy, so a plain detach fails and -force would unmount files still in use.
# Stop those processes first and name them, so a run that still cannot detach shows its holders;
# each signal gets five seconds before the next one.
stop_volume_users() {
  local mount_point=$1 signal pids pid tries
  for signal in TERM KILL; do
    pids=$(lsof -t -- "$mount_point" 2>/dev/null | sort -u) || true
    if [[ -z $pids ]]; then return 0; fi
    echo "Stopping processes that still use $mount_point with SIG$signal:" >&2
    ps -o pid=,comm= -p "$(paste -sd, - <<<"$pids")" >&2 || true
    while read -r pid; do kill -s "$signal" "$pid" 2>/dev/null || true; done <<<"$pids"
    for ((tries = 0; tries < 50; tries++)); do
      if ! lsof -t -- "$mount_point" >/dev/null 2>&1; then return 0; fi
      sleep 0.1
    done
  done
}

# Detaching the mounted volume ejects the whole RAM disk image and returns its memory.
detach_volume() {
  local target=$1 mount_point=$2
  stop_volume_users "$mount_point"
  if ! hdiutil detach "$target" >/dev/null 2>&1 && ! hdiutil detach -force "$target" >/dev/null; then
    echo "Could not detach the test RAM disk $mount_point; detach it with: hdiutil detach -force $mount_point" >&2
    return 1
  fi
}

# A run that was killed outright (SIGKILL, a crash, a closed terminal) never reaches its cleanup.
# Its volume name ends in the wrapper's PID; a volume whose owner is gone is released here, on every
# macOS run, so leftover memory does not accumulate while the RAM disk stays disabled. A volume
# whose PID is alive, possibly a concurrent run or a reused PID, is left alone.
release_stale_volumes() {
  local mount_point owner
  for mount_point in $(mount | awk -v prefix="/Volumes/$volume_prefix" \
    '$2 == "on" && index($3, prefix) == 1 && $3 ~ /-[0-9]+$/ { print $3 }'); do
    owner=${mount_point##*-}
    if ps -p "$owner" >/dev/null 2>&1; then continue; fi
    echo "Releasing a test RAM disk left by an earlier run: $mount_point" >&2
    detach_volume "$mount_point" "$mount_point" || true
  done
}

release_stale_volumes
if ! $use_ramdisk; then exec "$@"; fi

device=
mount_point=
detach() {
  if [[ -n $device ]]; then
    if [[ -n $mount_point ]]; then
      detach_volume "$device" "$mount_point" || return 1
    elif ! hdiutil detach "$device" >/dev/null 2>&1 && ! hdiutil detach -force "$device" >/dev/null; then
      echo "Could not detach the test RAM disk $device; detach it with: hdiutil detach -force $device" >&2
      return 1
    fi
    device=
    mount_point=
  fi
}
cleanup() {
  status=$?
  trap - EXIT
  if ! detach && [[ $status == 0 ]]; then status=1; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# 512-byte sectors. `diskutil image` replaces the deprecated `hdiutil attach -nomount` where present.
image="ram://$((size_mb * 2048))"
if diskutil image >/dev/null 2>&1; then
  attached=$(diskutil image attach --noMount "$image" 2>&1) || attached=
else
  attached=$(hdiutil attach -nomount "$image" 2>&1) || attached=
fi
device=$(awk '/^\/dev\/disk[0-9]+/ { print $1; exit }' <<<"$attached")
name="$volume_prefix$$"
if [[ -z $device ]] || ! diskutil erasevolume APFS "$name" "$device" >/dev/null ||
  ! mount | grep -F " on /Volumes/$name (" >/dev/null; then
  # Speed is the only loss, so the checks still run with the serial default.
  echo "Could not create a test RAM disk; running with TMPDIR=${TMPDIR:-/tmp} and serial test binaries" >&2
  detach
  "$@"
  exit
fi
mount_point="/Volumes/$name"
echo "Using a $size_mb MiB RAM disk for TMPDIR: $mount_point" >&2
export TMPDIR=$mount_point
export GCSM_TEST_JOBS=${GCSM_TEST_JOBS:-4}
"$@"
