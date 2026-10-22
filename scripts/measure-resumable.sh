#!/usr/bin/env bash
# Run once on macOS after cargo build --release --locked; no cloud or SDK calls.
# Requires curl, jq, Perl Time::HiRes, shasum and macOS time/ps. Uses loopback only,
# an isolated temporary catalog per case, one random 8 MiB client buffer on disk,
# at most eight upload clients and 2 GiB of server object data. Cleans up on exit.
# stdout is the evidence log; no payload, capability URL or local path is printed.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin ]] || { echo 'macOS is required' >&2; exit 1; }
server="$PWD/target/release/gcs-mimic"
[[ -x $server ]]
work=$(mktemp -d "${TMPDIR:?}/gcs-mimic-resources.XXXXXX")
pid='' timer='' sampler=''
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n $pid ]]; then kill -TERM "$pid" 2>/dev/null || true; fi
  if [[ -n $sampler ]]; then wait "$sampler" || true; fi
  if [[ -n $timer ]]; then wait "$timer" || status=1; fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
now() { perl -MTime::HiRes=time -e 'printf "%.6f\n", time'; }
cpu() { ps -p "$pid" -o time= | awk -F: '{print $1*60+$2}'; }
dd if=/dev/urandom of="$work/chunk" bs=1048576 count=8 2>/dev/null
chunk=8388608
curl_args=(--disable --silent --show-error --noproxy '*' --connect-timeout 5 --max-time 120)

upload() {
  local id=$1 total=$2 rate=$3 offset=0 end metrics status location
  local rate_args=(--limit-rate 0)
  [[ $rate == none ]] || rate_args=(--limit-rate "$rate")
  location=$(<"$work/location-$id")
  while ((offset < total)); do
    end=$((offset+chunk-1))
    metrics=$(curl "${curl_args[@]}" "${rate_args[@]}" -X PUT -T "$work/chunk" \
      -H 'Expect:' -H 'Content-Type: application/octet-stream' \
      -H "Content-Range: bytes $offset-$end/$total" \
      -D "$work/headers-$id" -o "$work/body-$id" \
      -w '%{http_code} %{time_total}' "$location")
    read -r status latency <<<"$metrics"
    if ((end+1 == total)); then
      [[ $status == 200 || $status == 201 ]]
      jq -e --arg size "$total" '.size == $size' "$work/body-$id" >/dev/null
      printf 'final id=%s status=%s latency_seconds=%s\n' "$id" "$status" "$latency" >"$work/final-$id"
    else
      [[ $status == 308 ]]
      tr -d '\r' <"$work/headers-$id" | grep -iq "^range: bytes=0-$end$"
    fi
    printf '%s\n' "$latency" >>"$work/latencies-$id"
    offset=$((end+1))
  done
}

for specification in 'small 16 1 none' 'medium 256 1 none' 'large 1024 1 none' 'slow 16 1 1M' 'concurrent 256 8 none'; do
  read -r label mib sessions rate <<<"$specification"
  total=$((mib*1048576))
  mkdir "$work/$label"
  address=$(perl -MIO::Socket::INET -e 'my $s=IO::Socket::INET->new(LocalAddr=>"127.0.0.1",LocalPort=>0,Listen=>1) or die $!; print "127.0.0.1:", $s->sockport')
  origin="http://$address"
  /usr/bin/time -l env -i PATH="$PATH" "$server" serve --listen "$address" \
    --api-base-url "$origin" --data-dir "$work/$label" \
    --max-object-bytes 2147483648 >"$work/server.log" 2>"$work/time.log" &
  timer=$!
  for ((attempt=0; attempt<100; attempt++)); do
    pid=$(pgrep -P "$timer" || true)
    ready=$(jq -Rr 'fromjson? | select(.event=="ready") | .listen' "$work/server.log")
    [[ -z $pid || $ready != "$address" ]] || break
    sleep 0.05
  done
  [[ -n $pid && $ready == "$address" ]]
  "$server" healthcheck --url "$origin/_internal/ready" >/dev/null
  curl "${curl_args[@]}" --fail -H 'Content-Type: application/json' \
    --data '{"name":"resource-bucket"}' "$origin/storage/v1/b?project=local-project" >/dev/null
  for ((id=1; id<=sessions; id++)); do
    status=$(curl "${curl_args[@]}" -X POST -H "X-Upload-Content-Length: $total" \
      -H 'Content-Length: 0' -D "$work/init" -o /dev/null -w '%{http_code}' \
      "$origin/upload/storage/v1/b/resource-bucket/o?uploadType=resumable&name=object-$id")
    [[ $status == 200 ]]
    location=$(awk 'tolower($1)=="location:" {gsub("\r", "", $2); print $2}' "$work/init")
    [[ $location == "$origin"/upload/storage/v1/* ]]
    printf '%s\n' "$location" >"$work/location-$id"
    : >"$work/latencies-$id"
  done
  baseline=$(ps -p "$pid" -o rss= | tr -d ' ')
  (
    while ps -p "$pid" -o rss=; do sleep 0.05; done
  ) >"$work/rss" 2>/dev/null &
  sampler=$!
  cpu_before=$(cpu)
  start=$(now)
  clients=()
  for ((id=1; id<=sessions; id++)); do upload "$id" "$total" "$rate" & clients+=("$!"); done
  for client in "${clients[@]}"; do wait "$client"; done
  finish=$(now)
  cpu_after=$(cpu)
  peak=$(awk 'v<$1 {v=$1} END {print v}' "$work/rss")
  disk=$(du -sk "$work/$label" | awk '{print $1}')
  printf '\ncase=%s object_MiB=%s sessions=%s chunk_MiB=8 rate=%s baseline_RSS_KiB=%s sampled_upload_peak_RSS_KiB=%s disk_KiB=%s\n' \
    "$label" "$mib" "$sessions" "$rate" "$baseline" "$peak" "$disk"
  awk -v start="$start" -v finish="$finish" -v before="$cpu_before" -v after="$cpu_after" -v mib="$mib" -v n="$sessions" \
    'BEGIN {printf "upload_wall_seconds=%.6f throughput_MiB_s=%.3f server_upload_CPU_seconds=%.2f CPU_seconds_per_GiB=%.3f\n", finish-start,mib*n/(finish-start),after-before,(after-before)/(mib*n/1024)}'
  for ((id=1; id<=sessions; id++)); do sed -n '1p' "$work/final-$id"; done
  for ((id=1; id<=sessions; id++)); do sed -n '1,$p' "$work/latencies-$id"; done |
    sort -n | awk '{a[NR]=$1} END {printf "chunk_request_count=%d latency_seconds_p50=%s p95=%s max=%s\n",NR,a[int((NR-1)*.5)+1],a[int((NR-1)*.95)+1],a[NR]}'
  expected=$({ for ((part=0; part<total/chunk; part++)); do dd if="$work/chunk" bs=1048576 2>/dev/null; done; } | shasum -a 256 | awk '{print $1}')
  for ((id=1; id<=sessions; id++)); do
    actual=$(curl "${curl_args[@]}" --fail "$origin/storage/v1/b/resource-bucket/o/object-$id?alt=media" | shasum -a 256 | awk '{print $1}')
    [[ $actual == "$expected" ]]
    printf 'sha256 id=%s expected=%s actual=%s MATCH\n' "$id" "$expected" "$actual"
  done
  kill -TERM "$pid"
  wait "$timer"
  wait "$sampler"
  pid='' timer='' sampler=''
  sed -n '1,30p' "$work/time.log"
  rm -rf "${work:?}/$label"
done
echo 'PASS: all chunk statuses, acknowledged ranges, final metadata sizes and download SHA-256 checks.'
