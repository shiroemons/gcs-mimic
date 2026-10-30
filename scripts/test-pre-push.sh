#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
cleanup() {
  if [[ $? != 0 && -f "$work/output" ]]; then cat "$work/output" >&2; fi
  rm -rf "$work"
}
trap cleanup EXIT
cp Justfile "$work/Justfile"
mkdir "$work/scripts"
cp scripts/pre-push-progress.sh "$work/scripts/pre-push-progress.sh"
# Periodic reports are exercised once below; the other runs only print stage transitions.
export GCSM_PRE_PUSH_PROGRESS_INTERVAL=0
# Replace expensive checks, retaining the real recipe graph and output pipeline.
for script in test-pre-push test-check; do
  printf 'exit 0\n' >"$work/scripts/$script.sh"
done
printf 'exec "$@"\n' >"$work/scripts/with-test-tmpdir.sh"
cat >"$work/scripts/check.sh" <<'SH'
set -euo pipefail
if [[ ${GCS_MIMIC_PRE_PUSH_TEST_FAIL:-} == check ]]; then exit 23; fi
sleep "${GCS_MIMIC_PRE_PUSH_TEST_SLEEP:-0}"
touch "$GCS_MIMIC_PRE_PUSH_TEST_STATE/check.done"
SH
cat >"$work/scripts/sdk-go.sh" <<'SH'
set -euo pipefail
test -f "$GCS_MIMIC_PRE_PUSH_TEST_STATE/check.done"
test "$*" = '--with-ruby --with-node --with-python --with-php'
if [[ ${GCS_MIMIC_PRE_PUSH_TEST_FAIL:-} == sdk ]]; then exit 23; fi
touch "$GCS_MIMIC_PRE_PUSH_TEST_STATE/sdk.done"
SH
cat >"$work/scripts/smoke-containers.sh" <<'SH'
set -euo pipefail
test -f "$GCS_MIMIC_PRE_PUSH_TEST_STATE/sdk.done"
test "$#" = 1
case "$1" in
  linux/amd64) arch=amd64; peer=arm64 ;;
  linux/arm64) arch=arm64; peer=amd64 ;;
  *) exit 2 ;;
esac
state=$GCS_MIMIC_PRE_PUSH_TEST_STATE
touch "$state/$arch.started"
trap 'touch "$state/$arch.cleaned"' EXIT
if [[ ${GCS_MIMIC_PRE_PUSH_TEST_SERIAL:-} == true ]]; then
  mkdir "$state/active"
  trap 'rmdir "$state/active"; touch "$state/$arch.cleaned"' EXIT
else
  # Neither worker can finish unless both have started; sequential execution fails.
  for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -f "$state/$peer.started" ]]; then break; fi
    sleep 0.05
  done
  test -f "$state/$peer.started"
fi
echo 'stdout marker'
echo 'stderr marker' >&2
if [[ ${GCS_MIMIC_PRE_PUSH_TEST_FAIL:-} == "$arch" || ${GCS_MIMIC_PRE_PUSH_TEST_FAIL:-} == both ]]; then
  exit 23
fi
if [[ ${GCS_MIMIC_PRE_PUSH_TEST_FAIL:-} == "$peer" ]]; then
  for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -f "$state/$peer.cleaned" ]]; then break; fi
    sleep 0.05
  done
  test -f "$state/$peer.cleaned"
  sleep 0.1
fi
touch "$state/$arch.passed"
SH

for failure in none amd64 arm64 both check sdk; do
  export GCS_MIMIC_PRE_PUSH_TEST_STATE="$work/$failure"
  export GCS_MIMIC_PRE_PUSH_TEST_FAIL=$failure
  mkdir "$GCS_MIMIC_PRE_PUSH_TEST_STATE"
  status=0
  just --justfile "$work/Justfile" --jobs 2 pre-push >"$work/output" 2>&1 || status=$?
  if [[ $failure == none ]]; then test "$status" = 0; else test "$status" = 23; fi
  if [[ $failure == none ]]; then
    # The first run has no history, so it can only count stages.
    grep -F '[pre-push 1/3 0%] Rust and shell checks started; no earlier run recorded' "$work/output" >/dev/null
    grep -F '[pre-push 3/3 100%] container checks took' "$work/output" >/dev/null
  else
    # Later runs estimate from the recorded stage durations and never claim completion on failure.
    grep -F 'based on the last run' "$work/output" >/dev/null
    if grep -F '100%]' "$work/output" >/dev/null; then exit 1; fi
  fi
  for arch in amd64 arm64; do
    if [[ $failure == check || $failure == sdk ]]; then
      test ! -e "$GCS_MIMIC_PRE_PUSH_TEST_STATE/$arch.started"
    else
      test -f "$GCS_MIMIC_PRE_PUSH_TEST_STATE/$arch.cleaned"
      grep -Fx "[linux/$arch] stdout marker" "$work/output" >/dev/null
      grep -Fx "[linux/$arch] stderr marker" "$work/output" >/dev/null
      if [[ $failure != "$arch" && $failure != both ]]; then
        test -f "$GCS_MIMIC_PRE_PUSH_TEST_STATE/$arch.passed"
      fi
    fi
  done
done

export GCS_MIMIC_PRE_PUSH_TEST_STATE="$work/serial"
export GCS_MIMIC_PRE_PUSH_TEST_FAIL=none
export GCS_MIMIC_PRE_PUSH_TEST_SERIAL=true
mkdir "$GCS_MIMIC_PRE_PUSH_TEST_STATE"
just --justfile "$work/Justfile" --jobs 1 pre-push >"$work/output" 2>&1
for arch in amd64 arm64; do
  test -f "$GCS_MIMIC_PRE_PUSH_TEST_STATE/$arch.passed"
  test -f "$GCS_MIMIC_PRE_PUSH_TEST_STATE/$arch.cleaned"
done

# A slow stage gets periodic reports, and the reporter stops with the run.
export GCS_MIMIC_PRE_PUSH_TEST_STATE="$work/periodic"
export GCS_MIMIC_PRE_PUSH_TEST_SERIAL=false GCS_MIMIC_PRE_PUSH_TEST_SLEEP=3 GCSM_PRE_PUSH_PROGRESS_INTERVAL=1
mkdir "$GCS_MIMIC_PRE_PUSH_TEST_STATE"
just --justfile "$work/Justfile" --jobs 2 pre-push >"$work/output" 2>&1
grep -E '^\[pre-push 1/3 [0-9]+%\] Rust and shell checks running for [0-9]+s \(last run took [0-9]+s\); about [0-9ms]+ left overall$' "$work/output" >/dev/null
test ! -e "$work/target/pre-push-progress/state"
echo 'PASS: pre-push reports stage progress and estimates, container recipes overlap, label both output streams, wait for cleanup, propagate failures and support serial execution'
