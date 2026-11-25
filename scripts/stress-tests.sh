#!/usr/bin/env bash
# Runs the Rust test suite repeatedly on a deliberately overloaded machine, to surface tests that
# depend on wall-clock timing before a release run does. Test binaries run in parallel and busy
# loops occupy every CPU, which is the condition that broke deadline tests on shared CI runners.
# Every round runs to the end; the script fails if any round failed and lists the failing binaries.
#
# GCSM_STRESS_ROUNDS (default 2) sets the number of rounds and GCSM_STRESS_JOBS the number of test
# binaries running at once. Each binary already runs one test per CPU, so N binaries and the busy
# loops put about N + 1 runnable threads on every CPU. GCSM_STRESS_JOBS therefore defaults to the
# CPU count capped at 4, the Linux CI runner's CPU count: a default that grew with the CPU count
# oversubscribed a 14-CPU workstation 15 times against the runner's 5, and timed out deadlines that
# no CI runner reaches.
set -euo pipefail

if [[ $# != 0 ]]; then echo 'Usage: bash scripts/stress-tests.sh' >&2; exit 2; fi
cd "$(dirname "$0")/.."

cpus=$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)
rounds=${GCSM_STRESS_ROUNDS:-2}
jobs=${GCSM_STRESS_JOBS:-$((cpus < 4 ? cpus : 4))}
for value in "$rounds" "$jobs"; do
  if [[ ! $value =~ ^[1-9][0-9]*$ ]]; then
    echo 'GCSM_STRESS_ROUNDS and GCSM_STRESS_JOBS must be positive integers' >&2
    exit 2
  fi
done

load=()
# shellcheck disable=SC2329 # invoked by the EXIT trap below
stop_load() {
  if ((${#load[@]})); then kill "${load[@]}" 2>/dev/null || true; fi
}
trap stop_load EXIT
for _ in $(seq 1 "$cpus"); do
  # A pure busy loop competes for CPU without touching the disk the tests measure.
  bash -c 'while :; do :; done' &
  load+=("$!")
done

failed=0
for round in $(seq 1 "$rounds"); do
  echo "== stress round $round/$rounds: $jobs parallel test binaries, $cpus busy loops"
  status=0
  GCSM_TEST_JOBS=$jobs bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh || status=$?
  if [[ $status != 0 ]]; then
    echo "== stress round $round failed (exit $status)"
    failed=1
  fi
done
exit "$failed"
