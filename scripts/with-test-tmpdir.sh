#!/usr/bin/env bash
# Runs a command with TMPDIR on a temporary macOS RAM disk, then detaches it. Tests keep their real
# sync calls, but macOS implements them with F_FULLFSYNC, which flushes the whole system disk and
# dominates test time. On a RAM disk the same calls finish quickly, so the wrapper also raises the
# default test binary concurrency. Other platforms, or GCS_MIMIC_TEST_RAMDISK=0, run the command
# unchanged with serial test binaries.
set -euo pipefail

if [[ $# == 0 ]]; then
  echo 'Usage: bash scripts/with-test-tmpdir.sh COMMAND [ARGUMENT...]' >&2
  exit 2
fi
case ${GCS_MIMIC_TEST_RAMDISK:-1} in
  0) exec "$@" ;;
  1) ;;
  *) echo 'GCS_MIMIC_TEST_RAMDISK must be 0 or 1' >&2; exit 2 ;;
esac
if [[ $(uname -s) != Darwin ]]; then exec "$@"; fi
size_mb=${GCS_MIMIC_TEST_RAMDISK_MB:-4096}
if [[ ! $size_mb =~ ^[1-9][0-9]*$ ]]; then
  echo 'GCS_MIMIC_TEST_RAMDISK_MB must be a positive integer' >&2
  exit 2
fi

device=
detach() {
  if [[ -n $device ]]; then
    # A test process that outlived its harness must not keep the RAM disk attached.
    if ! hdiutil detach "$device" >/dev/null 2>&1 && ! hdiutil detach -force "$device" >/dev/null; then
      echo "Could not detach the test RAM disk $device; detach it with: hdiutil detach -force $device" >&2
      return 1
    fi
    device=
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
name="gcs-mimic-tmp-$$"
mount_point="/Volumes/$name"
if [[ -z $device ]] || ! diskutil erasevolume APFS "$name" "$device" >/dev/null ||
  ! mount | grep -F " on $mount_point (" >/dev/null; then
  # Speed is the only loss, so the checks still run with the serial default.
  echo "Could not create a test RAM disk; running with TMPDIR=${TMPDIR:-/tmp} and serial test binaries" >&2
  detach
  "$@"
  exit
fi
echo "Using a $size_mb MiB RAM disk for TMPDIR: $mount_point" >&2
export TMPDIR=$mount_point
export GCS_MIMIC_TEST_JOBS=${GCS_MIMIC_TEST_JOBS:-4}
"$@"
