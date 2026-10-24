#!/usr/bin/env bash
# Native macOS baseline only; prepare before reserving an exclusive measurement window.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
driver="$PWD/target/release/examples/onboarding-state-benchmark"
server="$PWD/target/release/gcs-mimic"
pid='' sampler='' driver_pid=''

cleanup() {
  local status=$? child attempt
  trap - EXIT INT TERM
  for child in "$driver_pid" "$sampler" "$pid"; do
    if [[ -n $child ]] && kill -0 "$child" 2>/dev/null; then kill -TERM "$child"; fi
  done
  for ((attempt=0; attempt<100; attempt++)); do
    local alive=false
    for child in "$driver_pid" "$sampler" "$pid"; do
      if [[ -n $child ]] && kill -0 "$child" 2>/dev/null; then alive=true; fi
    done
    [[ $alive == true ]] || break
    sleep 0.05
  done
  for child in "$driver_pid" "$sampler" "$pid"; do
    if [[ -n $child ]]; then
      if kill -0 "$child" 2>/dev/null; then kill -KILL "$child"; fi
      wait "$child" 2>/dev/null || true
    fi
  done
  exit "$status"
}
now() { perl -MTime::HiRes=time -e 'printf "%.6f\n", time'; }
cpu() { ps -p "$pid" -o time= | awk -F: '{print $1*60+$2}'; }
wait_ready() {
  local attempts=$1 attempt ready=''
  for ((attempt=0; attempt<attempts; attempt++)); do
    ready=$(jq -Rr 'fromjson? | select(.event=="ready") | .listen' "$work/server.log")
    if [[ -n $ready ]]; then address=$ready; return; fi
    kill -0 "$pid" || return 1
    sleep 0.01
  done
  echo 'server did not become ready before the bounded deadline' >&2
  return 1
}
start_server() {
  started=$(now)
  env -i PATH="$PATH" "$server" serve --scheme http \
    --listen 127.0.0.1:0 --api-base-url http://127.0.0.1:4443 --data-dir "$1" \
    >"$work/server.log" 2>"$work/server.err" &
  pid=$!
  wait_ready 12000
  finished=$(now)
  "$server" healthcheck --url "http://$address/_internal/ready" >/dev/null
}
stop_server() {
  kill -TERM "$pid"
  local attempt
  for ((attempt=0; attempt<100; attempt++)); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.05
  done
  if kill -0 "$pid" 2>/dev/null; then
    echo 'server failed to stop within five seconds' >&2
    return 1
  fi
  wait "$pid"
  pid=''
  jq -e 'select(.event=="stopped")' "$work/server.log" >/dev/null
}
measure_driver() {
  local label=$1 before after peak disk
  shift
  : >"$work/rss"
  (
    while kill -0 "$pid" 2>/dev/null; do
      ps -p "$pid" -o rss= >>"$work/rss"
      sleep 0.05
    done
  ) &
  sampler=$!
  before=$(cpu)
  "$driver" "$label" "$address" "$@" >"$work/driver.jsonl" &
  driver_pid=$!
  wait "$driver_pid"
  driver_pid=''
  after=$(cpu)
  if [[ $label == publish ]]; then last_generation=$(jq -r '.last_generation' "$work/driver.jsonl"); fi
  kill -TERM "$sampler"
  wait "$sampler" || [[ $? == 143 ]]
  sampler=''
  peak=$(awk 'p<$1 {p=$1} END {print p+0}' "$work/rss")
  disk=$(du -sk "$work/small-$round" | awk '{print $1}')
  jq -c --argjson round "$round" --argjson before "$before" --argjson after "$after" \
    --argjson peak "$peak" --argjson disk "$disk" \
    '. + {round:$round,server_cpu_seconds:($after-$before),server_cpu_seconds_per_gib:(($after-$before)/(.bytes/1073741824)),sampled_RSS_KiB:$peak,allocated_disk_KiB:$disk,cgroup_memory:null}' "$work/driver.jsonl"
}
filesystem_type() {
  local device filesystem
  device=$(df -P "$1" | awk 'NR==2 {print $1}')
  filesystem=$(mount | awk -v device="$device" '$1==device {split($0,a,"("); split(a[2],b,","); print b[1]}')
  [[ $filesystem =~ ^[[:alnum:]_-]+$ && $filesystem != Directory ]] || return 1
  printf '%s\n' "$filesystem"
}
main() {
  [[ $(uname -s) == Darwin ]] || { echo 'native macOS is required' >&2; exit 2; }
  [[ $# == 2 && ( $1 == prepare || $1 == measure ) ]] || {
    echo 'usage: bash scripts/measure-onboarding-state.sh prepare|measure WORK_DIR' >&2
    exit 2
  }
  work=$2
  [[ -x $driver && -x $server && -d $work ]]
  local source_digest
  source_digest=$(git rev-parse 60a4f03)
  git diff --quiet 60a4f03 -- src Cargo.toml Cargo.lock rust-toolchain.toml
  if [[ $1 == prepare ]]; then
    [[ ! -e $work/prepared ]]
    "$driver" prepare "$work/startup-10000" 10000
    "$driver" prepare "$work/startup-100000" 100000
    for round in 1 2 3 4 5; do "$driver" prepare "$work/small-$round" 0; done
    printf '%s\n' "$source_digest" >"$work/prepared"
    return
  fi
  [[ $(<"$work/prepared") == "$source_digest" && ! -e $work/measured ]]
  touch "$work/measured"
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  printf 'source=%s profile=native-macos-http-permissive-native-wal-full concurrency=1 repetitions=5\n' "$source_digest"
  printf 'cache=not-evicted CPU=server-ps-0.01s RSS=server-50ms cgroup=UNMEASURED disk=phase-end-not-high-water startup=observed-ready-poll-10ms-plus-command-overhead\n'
  rustc -Vv
  cargo -V
  uname -sm
  sw_vers
  sysctl -n machdep.cpu.brand_string hw.memsize hw.logicalcpu
  local filesystem
  filesystem=$(filesystem_type "$work")
  printf 'filesystem_type=%s\n' "$filesystem"
  shasum -a 256 Cargo.lock rust-toolchain.toml target/release/gcs-mimic target/release/examples/onboarding-state-benchmark
  for round in 1 2 3 4 5; do
    start_server "$work/small-$round"
    measure_driver publish 10000 0
    measure_driver overwrite 10000 "$last_generation"
    measure_driver upload
    measure_driver download
    stop_server
    for count in 10000 100000; do
      start_server "$work/startup-$count"
      local seconds startup_cpu rss disk
      seconds=$(awk -v before="$started" -v after="$finished" 'BEGIN {printf "%.6f",after-before}')
      startup_cpu=$(cpu)
      rss=$(ps -p "$pid" -o rss= | tr -d ' ')
      disk=$(du -sk "$work/startup-$count" | awk '{print $1}')
      jq -cn --argjson round "$round" --argjson count "$count" --argjson seconds "$seconds" \
        --argjson cpu "$startup_cpu" --argjson rss "$rss" --argjson disk "$disk" \
        '{round:$round,workload:("startup-"+($count|tostring)),objects:$count,wall_seconds:$seconds,server_cpu_seconds:$cpu,ready_RSS_KiB:$rss,allocated_disk_KiB:$disk,errors:0,cgroup_memory:null}'
      stop_server
    done
  done
  echo 'PASS: five repetitions, checked response counts, checksums, exact streamed bytes and readiness; sync/cgroups unmeasured.'
}
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
