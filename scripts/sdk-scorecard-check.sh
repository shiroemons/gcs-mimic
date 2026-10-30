#!/usr/bin/env bash
# Runs the SDK scorecard probes against gcs-mimic and fails unless every case passes.
# The scorecard only scores, so this gate is what checks the Node.js, PHP and Python SDKs;
# it also reruns the Go and Ruby probes because a scored run requires all five SDKs.
# Results stay in a temporary directory, never in sdk-scorecard/results, and are kept on
# failure for inspection.
set -euo pipefail

if [[ $# != 0 ]]; then echo 'Usage: bash scripts/sdk-scorecard-check.sh' >&2; exit 2; fi

cd "$(dirname "$0")/.."
work=$(mktemp -d "${TMPDIR:-/tmp}/gcs-mimic-sdk-scorecard.XXXXXX")
results=$work/results
keep=false
cleanup() {
  if [[ $keep == true ]]; then
    echo "Scorecard results kept in $results" >&2
  else
    rm -rf "$work"
  fi
}
trap cleanup EXIT

keep=true
bash sdk-scorecard/run.sh --out "$results" gcs-mimic
failures=$(jq -r 'select(.pass != true) | "\(.sdk) \(.id)"' "$results"/gcs-mimic.*.jsonl)
if [[ -n $failures ]]; then
  echo 'FAIL: SDK scorecard cases that did not pass against gcs-mimic:' >&2
  echo "$failures" >&2
  exit 1
fi
cases=$(jq -s length "$results"/gcs-mimic.*.jsonl | awk '{total += $1} END {print total}')
keep=false
echo "PASS: all $cases SDK scorecard cases (Go, Ruby, Node.js, PHP, Python) against gcs-mimic"
