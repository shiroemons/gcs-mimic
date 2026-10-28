#!/usr/bin/env bash
# Runs every workspace test that `cargo test --workspace` runs, with test binaries in a bounded pool.
# Cargo runs test binaries one at a time while most of their time waits on file synchronization, so
# the doctests and the SDK server build share that wait. GCS_MIMIC_TEST_JOBS (default 1) bounds the
# pool: concurrent syncs on a system disk delay unrelated processes enough to break wall-clock
# deadline tests, so scripts/with-test-tmpdir.sh raises the default only on a RAM-backed TMPDIR.
set -euo pipefail

if [[ ${1:-} == --run-binary ]]; then
  # Pool worker. It always succeeds so xargs starts every binary; the status file records failures.
  out=$2 manifest=$3 exe=$4 status=0
  name=${exe##*/}
  # Cargo runs each test binary from its package root.
  (cd "${manifest%/*}" && CARGO_MANIFEST_DIR=${manifest%/*} "$exe") >"$out/$name.log" 2>&1 || status=$?
  echo "$status" >"$out/$name.status"
  summary=$(grep -E '^test result:' "$out/$name.log" | tail -n 1 || true)
  if [[ $status == 0 ]]; then
    echo "PASS ${name%-*}: ${summary:-no test result}"
  else
    echo "FAIL ${name%-*} (exit $status)"
  fi
  exit 0
fi
if [[ $# != 0 ]]; then echo 'Usage: bash scripts/test-rust.sh' >&2; exit 2; fi

cd "$(dirname "$0")/.."
script=$PWD/scripts/test-rust.sh
jobs=${GCS_MIMIC_TEST_JOBS:-1}
if [[ ! $jobs =~ ^[1-9][0-9]*$ ]]; then
  echo 'GCS_MIMIC_TEST_JOBS must be a positive integer' >&2
  exit 2
fi
work=$(mktemp -d "${TMPDIR:-/tmp}/gcs-mimic-test-rust.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/out"

cargo test --workspace --locked --offline --no-run --message-format=json-render-diagnostics >"$work/build.json"
jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable != null)
  | [.manifest_path, .executable] | @tsv' "$work/build.json" >"$work/binaries"
if [[ ! -s $work/binaries ]]; then
  echo 'cargo test --no-run reported no test executables' >&2
  exit 1
fi

# The SDK recipe runs the binary built without dev-dependency features; building it here keeps
# that compilation inside the synchronization wait instead of the SDK stage.
(cargo build --locked --offline && cargo test --workspace --locked --offline --doc) >"$work/lane.log" 2>&1 &
lane_pid=$!

# Larger binaries hold more tests, so they start first and the pool drains with short ones.
# A pool failure is recorded rather than fatal so the lane is always awaited.
pool_status=0
while IFS=$'\t' read -r manifest exe; do
  printf '%s\t%s\t%s\n' "$(wc -c <"$exe")" "$manifest" "$exe"
done <"$work/binaries" | sort -rn | cut -f 2- | tr '\t\n' '\0\0' |
  xargs -0 -n 2 -P "$jobs" bash "$script" --run-binary "$work/out" || pool_status=$?

lane_status=0
wait "$lane_pid" || lane_status=$?
cat "$work/lane.log"

status=$lane_status
if [[ $status == 0 && $pool_status != 0 ]]; then status=$pool_status; fi
count=0
while IFS=$'\t' read -r _ exe; do
  name=${exe##*/}
  count=$((count + 1))
  binary_status=$(cat "$work/out/$name.status" 2>/dev/null || echo 'missing')
  if [[ $binary_status != 0 ]]; then
    echo "--- ${name%-*} (exit $binary_status) ---" >&2
    cat "$work/out/$name.log" >&2 2>/dev/null || true
    if [[ $status == 0 ]]; then
      if [[ $binary_status =~ ^[0-9]+$ ]]; then status=$binary_status; else status=1; fi
    fi
  fi
done <"$work/binaries"
if [[ $status != 0 ]]; then
  echo "FAIL: Rust tests failed (doctest/build lane exit $lane_status)" >&2
  exit "$status"
fi
echo "PASS: $count test binaries and doctests with $jobs concurrent binaries"
