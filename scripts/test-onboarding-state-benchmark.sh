#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == upload || ${1:-} == download ]]; then
  printf '{"workload":"%s-1gib","bytes":1073741824,"errors":0}\n' "$1"
  exit
fi
source "$(dirname "$0")/measure-onboarding-state.sh"
test_script="$PWD/scripts/test-onboarding-state-benchmark.sh"
testwork=$(mktemp -d "${TMPDIR:?}/gcs-mimic-benchmark-check.XXXXXX")
trap 'rm -rf "$testwork"' EXIT
for mode in timeout interrupt; do
  work="$testwork/$mode"
  mkdir "$work"
  before=$(date +%s)
  (
    trap cleanup EXIT
    trap 'exit 143' TERM
    "$driver" hold-catalog "$work/catalog" >"$work/server.log" 2>"$work/server.err" &
    pid=$!
    printf '%s\n' "$pid" >"$work/child.pid"
    if [[ $mode == timeout ]]; then wait_ready 20; else wait_ready 10000; fi
  ) &
  owner=$!
  for ((attempt=0; attempt<200; attempt++)); do
    if [[ -e $work/server.log ]] && rg -q 'catalog opened' "$work/server.log"; then break; fi
    sleep 0.01
  done
  rg -q 'catalog opened' "$work/server.log"
  if [[ $mode == interrupt ]]; then kill -TERM "$owner"; fi
  status=0
  wait "$owner" || status=$?
  if [[ $mode == interrupt ]]; then [[ $status == 143 ]]; else [[ $status == 1 ]]; fi
  child=$(<"$work/child.pid")
  ! kill -0 "$child" 2>/dev/null
  (( $(date +%s) - before < 8 ))
  "$driver" check-catalog "$work/catalog" >/dev/null
done
work="$testwork/resources"
mkdir -p "$work/small-1"
round=1
address=127.0.0.1:1
pid=$$
printf '0\n' >"$work/cpu-index"
cpu() {
  local index
  index=$(<"$work/cpu-index")
  printf '%s\n' "$((index+1))" >"$work/cpu-index"
  case $index in 0) echo 0;; 1) echo 1;; 2) echo 10;; 3) echo 12;; *) return 1;; esac
}
driver=$test_script
measure_driver upload >"$work/upload.json"
measure_driver download >"$work/download.json"
jq -e '.workload=="upload-1gib" and .server_cpu_seconds==1' "$work/upload.json" >/dev/null
jq -e '.workload=="download-1gib" and .server_cpu_seconds==2' "$work/download.json" >/dev/null
filesystem=$(filesystem_type "$work")
[[ -n $filesystem && $filesystem != Directory ]]
df() { printf 'Filesystem blocks used available capacity mounted\n/dev/example 1 1 1 1%% /\n'; }
mount() { printf '/dev/example on / (apfs, local)\n'; }
[[ $(filesystem_type "$work") == apfs ]]
mount() { printf '/dev/example on / (Directory, local)\n'; }
if filesystem_type "$work"; then echo 'invalid filesystem type was accepted' >&2; exit 1; fi
jq -cn 'range(1;6) as $round | ["download-1gib","small-overwrite","small-publish","startup-10000","startup-100000","upload-1gib"][] as $workload | {round:$round,workload:$workload,wall_seconds:$round,errors:0,requests:(if $workload|startswith("small") then 10000 else 1 end),bytes:(if $workload|startswith("small") then 10240000 else 1073741824 end)}' |
  jq -Rs -f scripts/summarize-onboarding-state.jq >"$work/summary.json"
jq -e '(.workloads|length)==6 and all(.workloads[]; .thresholds.wall_seconds.median==3 and .thresholds.wall_seconds.median_upper_bound==11)' "$work/summary.json" >/dev/null
{
  printf 'source=baseline\n68719476736\n14\nnull\ntrue\n[]\n"metadata"\n'
  jq -c '.workloads[].samples_by_round[]' "$work/summary.json"
} | jq -Rs -f scripts/summarize-onboarding-state.jq >"$work/metadata.json"
jq -e '(.workloads|length)==6 and all(.workloads[]; .thresholds.wall_seconds.median==3 and .thresholds.wall_seconds.median_upper_bound==11)' "$work/metadata.json" >/dev/null
jq -c '.workloads[].samples_by_round[] | .server_cpu_seconds=0.05 | .server_cpu_seconds_per_gib=5.24288' "$work/summary.json" |
  jq -Rs -f scripts/summarize-onboarding-state.jq >"$work/quantized.json"
jq -e 'all(.workloads[]; (.thresholds.server_cpu_seconds.calibration|startswith("PENDING")) and (if .workload|startswith("startup-") then (.not_applicable_metrics|index("server_cpu_seconds_per_gib"))!=null else .thresholds.server_cpu_seconds_per_gib.calibration|startswith("PENDING") end))' "$work/quantized.json" >/dev/null
jq -c '.workloads[].samples_by_round[] | .server_cpu_seconds=([0.05,0.2,0.3,0.4,0.5][.round-1]) | .server_cpu_seconds_per_gib=(.server_cpu_seconds*102.4) | .wall_seconds=10 | .allocated_disk_KiB=1024 | if .round==3 then del(.allocated_disk_KiB) else . end' "$work/summary.json" |
  jq -Rs -f scripts/summarize-onboarding-state.jq >"$work/missing.json"
jq -e 'all(.workloads[]; (.thresholds.server_cpu_seconds.calibration|startswith("PENDING")) and (if .workload|startswith("startup-") then true else .thresholds.server_cpu_seconds_per_gib.calibration|startswith("PENDING") end) and .thresholds.allocated_disk_KiB.missing_rounds==[3] and .thresholds.wall_seconds.allowed_regression_fraction==0.1 and .thresholds.wall_seconds.median_upper_bound==11)' "$work/missing.json" >/dev/null
jq -e 'all(.workloads[]; if .workload|startswith("startup-") then (.not_applicable_metrics|index("p50_seconds"))!=null and (.thresholds|has("p50_seconds")|not) else .thresholds.p50_seconds.missing_rounds==[1,2,3,4,5] end)' "$work/summary.json" >/dev/null
if printf '{}\n' | jq -Rs -f scripts/summarize-onboarding-state.jq >"$work/invalid.json"; then
  echo 'incomplete repetitions were accepted' >&2
  exit 1
fi
echo 'PASS: pre-ready timeout/interruption releases child/catalog ownership, directional CPU intervals stay separate, filesystem types and five-run calibration are validated.'
