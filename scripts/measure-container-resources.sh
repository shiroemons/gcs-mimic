#!/usr/bin/env bash
# The observer stays outside the target cgroup; the distroless runtime is unchanged.
set -euo pipefail

# The pinned builder has awk but not jq; probe output uses only validated numbers.
probe() {
  local group stat ticks rss current peak cache usage oom killed maximum swap quota hz stamp value
  [[ $(< /proc/1/comm) == gcs-mimic ]]
  group=$(awk -F: '$1==0 {print $3}' /proc/1/cgroup)
  [[ $group == /* && $group != / && $group != *..* ]]
  group="/sys/fs/cgroup$group"
  read -r maximum <"$group/memory.max"
  read -r swap <"$group/memory.swap.max"
  read -r quota <"$group/cpu.max"
  [[ $maximum == 134217728 && $swap == 0 && $quota == '100000 100000' ]]
  [[ $(<"$group/pids.max") == 64 ]]
  stat=$(< /proc/1/stat)
  ticks=$(awk '{print $12+$13}' <<<"${stat##*) }")
  rss=$(awk '$1=="VmRSS:" {print $2*1024}' /proc/1/status)
  read -r current <"$group/memory.current"
  read -r peak <"$group/memory.peak"
  cache=$(awk '$1=="file" {print $2}' "$group/memory.stat")
  usage=$(awk '$1=="usage_usec" {print $2}' "$group/cpu.stat")
  oom=$(awk '$1=="oom" {print $2}' "$group/memory.events")
  killed=$(awk '$1=="oom_kill" {print $2}' "$group/memory.events")
  hz=$(getconf CLK_TCK)
  stamp=$(date +%s.%N)
  for value in "$ticks" "$rss" "$current" "$peak" "$cache" "$usage" "$oom" "$killed" "$hz"; do
    [[ $value =~ ^[0-9]+$ ]] || return 1
  done
  [[ $stamp =~ ^[0-9]+\.[0-9]+$ ]]
  printf '{"time":%s,"rss_bytes":%s,"cgroup_bytes":%s,"cgroup_peak_bytes":%s,"file_cache_bytes":%s,"cgroup_cpu_usec":%s,"process_cpu_ticks":%s,"hz":%s,"oom":%s,"oom_kill":%s}\n' \
    "$stamp" "$rss" "$current" "$peak" "$cache" "$usage" "$ticks" "$hz" "$oom" "$killed"
}

summarize() {
  jq -se --argjson first "$1" --argjson last "$2" '
    def valid: all(.[]; type == "object" and
      all(.time,.rss_bytes,.cgroup_bytes,.cgroup_peak_bytes,.file_cache_bytes,
          .cgroup_cpu_usec,.process_cpu_ticks,.hz,.oom,.oom_kill; type == "number" and . >= 0)
      and .hz > 0 and .rss_bytes > 0 and .cgroup_bytes > 0
      and .cgroup_peak_bytes >= .cgroup_bytes);
    if ([$first,$last] + . | valid) then . else error("invalid probe records") end
    | [$first] + [.[] | select(.time >= $first.time and .time <= $last.time)] + [$last]
    | sort_by(.time)
    | if (valid and length >= 3 and $last.time > $first.time and
          all(.[]; .hz == $first.hz and .oom == 0 and .oom_kill == 0)) then . else error("invalid or missing probe records") end
    | if ($last.cgroup_cpu_usec >= $first.cgroup_cpu_usec and
          $last.process_cpu_ticks >= $first.process_cpu_ticks)
      then . else error("CPU counter regressed") end
    | {wall_seconds:($last.time-$first.time),samples:length,
       process_cpu_seconds:(($last.process_cpu_ticks-$first.process_cpu_ticks)/$first.hz),
       cgroup_cpu_seconds:(($last.cgroup_cpu_usec-$first.cgroup_cpu_usec)/1000000),
       baseline_rss_bytes:$first.rss_bytes,sampled_peak_rss_bytes:(map(.rss_bytes)|max),
       baseline_cgroup_bytes:$first.cgroup_bytes,sampled_peak_cgroup_bytes:(map(.cgroup_bytes)|max),
       cumulative_cgroup_peak_bytes:$last.cgroup_peak_bytes,sampled_peak_file_cache_bytes:(map(.file_cache_bytes)|max),
       oom:$last.oom,oom_kill:$last.oom_kill}'
}

quantiles() {
  sort -n "$1" | awk '{a[NR]=$1} END {if (!NR) exit 1; printf "requests=%d p50_seconds=%s p95_seconds=%s p99_seconds=%s max_seconds=%s\n",NR,a[int((NR-1)*.50)+1],a[int((NR-1)*.95)+1],a[int((NR-1)*.99)+1],a[NR]}'
}

start_container() {
  local address=$1
  origin="http://$address"
  container=$(docker run -d --read-only --memory 128m --memory-swap 128m --cpus 1 --pids-limit 64 --ulimit nofile=1024:1024 \
    --no-healthcheck -p "$address:4443" --mount "type=volume,src=$volume,dst=/var/lib/gcs-mimic" \
    "$image" serve --scheme http --listen 0.0.0.0:4443 --api-base-url "$origin" \
    --data-dir /var/lib/gcs-mimic --max-object-bytes 2147483648)
  [[ $(docker port "$container" 4443/tcp) == "$address" ]]
}

cleanup_all() {
  local original=$1 failed=0 result
  if [[ -n $observer ]]; then
    if docker rm -f "$observer" >/dev/null 2>&1; then :; else
      result=$?; printf 'cleanup_failed resource=observer exit=%s\n' "$result" >&2; failed=1
    fi
  fi
  if [[ -n $container ]]; then
    if docker rm -f "$container" >/dev/null 2>&1; then :; else
      result=$?; printf 'cleanup_failed resource=target exit=%s\n' "$result" >&2; failed=1
    fi
  fi
  if [[ -n $volume ]]; then
    if docker volume rm "$volume" >/dev/null 2>&1; then :; else
      result=$?; printf 'cleanup_failed resource=volume exit=%s\n' "$result" >&2; failed=1
    fi
  fi
  if rm -rf "$work"; then :; else
    result=$?; printf 'cleanup_failed resource=work exit=%s\n' "$result" >&2; failed=1
  fi
  if ((original != 0)); then return "$original"; fi
  return "$failed"
}

download_diagnostics() {
  awk '/^curl: \([0-9]+\)/ {line=$0; sub(/^curl: \(/,"",line); sub(/\).*/,"",line); print "download_curl_error_code=" line}
    /^http_status=[0-9][0-9][0-9] request_seconds=[0-9.]+$/ {print}' "$1"
}

self_check() {
  local first last middle result
  first='{"time":1,"rss_bytes":1024,"cgroup_bytes":8192,"cgroup_peak_bytes":16384,"file_cache_bytes":4096,"cgroup_cpu_usec":1000000,"process_cpu_ticks":100,"hz":100,"oom":0,"oom_kill":0}'
  last=$(jq -c '.time=3 | .cgroup_cpu_usec=3500000 | .process_cpu_ticks=300' <<<"$first")
  middle=$(jq -c '.time=2 | .rss_bytes=2048 | .cgroup_bytes=12288' <<<"$first")
  result=$(printf '%s\n%s\n' "$middle" "$(jq -c '.time=4 | .rss_bytes=99999' <<<"$last")" | summarize "$first" "$last")
  jq -e '.wall_seconds==2 and .process_cpu_seconds==2 and .cgroup_cpu_seconds==2.5 and .sampled_peak_rss_bytes==2048 and .sampled_peak_cgroup_bytes==12288 and .cumulative_cgroup_peak_bytes==16384 and .samples==3' <<<"$result" >/dev/null
  if printf '%s\n' "$middle" | summarize "$first" "$(jq -c '.process_cpu_ticks=0' <<<"$last")" >/dev/null 2>&1; then return 1; fi
  if printf '%s\n' "$(jq -c 'del(.rss_bytes)' <<<"$middle")" | summarize "$first" "$last" >/dev/null 2>&1; then return 1; fi
  if printf '%s\n' "$(jq -c '.oom_kill=1' <<<"$middle")" | summarize "$first" "$last" >/dev/null 2>&1; then return 1; fi
  if printf '' | summarize "$first" "$last" >/dev/null 2>&1; then return 1; fi
  for invalid in 'del(.time)' '.time=null' '.time="invalid"'; do
    if printf '%s\n' "$(jq -c "$invalid" <<<"$middle")" | summarize "$first" "$last" >/dev/null 2>&1; then return 1; fi
  done
  # Concurrent samples have no CPU read ordering; cache/current are separate reads.
  result=$(printf '%s\n' "$(jq -c '.cgroup_cpu_usec=9000000 | .process_cpu_ticks=900 | .file_cache_bytes=20000' <<<"$middle")" | summarize "$first" "$last")
  jq -e '.process_cpu_seconds==2 and .cgroup_cpu_seconds==2.5 and .sampled_peak_file_cache_bytes==20000' <<<"$result" >/dev/null
  [[ $(quantiles <(printf '9\n1\n5\n2\n3\n')) == 'requests=5 p50_seconds=3 p95_seconds=5 p99_seconds=5 max_seconds=9' ]]
  (
    image=self-check-image volume=self-check-volume
    docker() {
      case $1 in
        run)
          local previous='' published='' advertised='' argument
          for argument in "$@"; do
            [[ $previous != -p ]] || published=$argument
            [[ $previous != --api-base-url ]] || advertised=$argument
            previous=$argument
          done
          [[ $published == 127.0.0.1:43123:4443 && $advertised == http://127.0.0.1:43123 ]] || return 1
          echo self-check-container ;;
        port) [[ $2 == self-check-container && $3 == 4443/tcp ]]; echo 127.0.0.1:43123 ;;
        *) return 1 ;;
      esac
    }
    start_container 127.0.0.1:43123
    [[ $origin == http://127.0.0.1:43123 ]]
  )
  (
    observer=observer container=target volume=volume work=self-check-work
    local calls=''
    docker() { calls+=" $*"; [[ $* != 'rm -f observer' ]] || return 7; }
    rm() { calls+=" rm $*"; return 0; }
    if cleanup_all 1 >/dev/null 2>&1; then return 1; else [[ $? == 1 ]]; fi
    [[ $calls == ' rm -f observer rm -f target volume rm volume rm -rf self-check-work' ]]
    calls=''
    if cleanup_all 0 >/dev/null 2>&1; then return 1; else [[ $? == 1 ]]; fi
    [[ $calls == ' rm -f observer rm -f target volume rm volume rm -rf self-check-work' ]]
  )
  [[ $(download_diagnostics <(printf 'curl: (22) secret-capability-url\nhttp_status=404 request_seconds=0.123\n')) == $'download_curl_error_code=22\nhttp_status=404 request_seconds=0.123' ]]
  echo 'PASS: resource arithmetic, non-atomic samples, malformed time, OOM, boundary CPU regression, quantiles, advertised origin and cleanup self-checks'
}

case ${1:-} in
  --probe) probe; exit ;;
  --observe) while true; do probe; sleep 0.05; done ;;
  --self-check) self_check; exit ;;
esac

[[ $# == 2 && $2 =~ ^[a-f0-9]{40}$ ]] || { echo 'usage: measure-container-resources.sh IMAGE SOURCE_SHA | --self-check' >&2; exit 2; }
cd "$(dirname "$0")/.."
image=$(docker image inspect "$1" --format '{{.Id}}')
source_sha=$2
[[ $(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}') == "$source_sha" ]]
git diff --quiet "$source_sha" -- src Cargo.toml Cargo.lock rust-toolchain.toml Dockerfile distribution xtask .dockerignore compatibility/upstream/json-schemas.json
observer_image=$(awk '$1=="FROM" {print $2; exit}' Dockerfile)
docker image inspect "$observer_image" >/dev/null
work=$(mktemp -d "${TMPDIR:?}/gcs-mimic-container-resources.XXXXXX")
container='' observer='' volume='' phase=setup round=0
cleanup() {
  local status=$? state
  trap - EXIT
  if ((status != 0)); then
    printf 'FAIL: round=%s phase=%s original_exit=%s\n' "$round" "$phase" "$status" >&2
    if [[ -s $work/download-time ]]; then
      download_diagnostics "$work/download-time" >&2
    fi
    if [[ -n $container ]] && state=$(docker inspect "$container" --format '{{.State.Status}} {{.State.ExitCode}} {{.State.OOMKilled}}' 2>/dev/null); then
      printf 'target_status_exit_oom=%s\n' "$state" >&2
    fi
  fi
  if cleanup_all "$status"; then exit 0; else exit "$?"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
curl_args=(--disable --silent --show-error --http1.1 --noproxy '*' --connect-timeout 5 --max-time 180)
boundary() { docker exec "$observer" bash /measure.sh --probe; }
phase_start() { first=$(boundary); }
phase_end() {
  local last data
  last=$(boundary)
  docker logs "$observer" >"$work/samples"
  data=$(summarize "$first" "$last" <"$work/samples")
  jq -c --argjson round "$round" --arg phase "$1" '. + {round:$round,phase:$phase}' <<<"$data"
}
printf 'source=%s image_id=%s observer=%s repetitions=5 idle_seconds=60 bytes=1073741824 concurrency=1\n' "$source_sha" "$image" "$observer_image"
docker image inspect "$image" --format 'image_repo_digests={{json .RepoDigests}} architecture={{.Architecture}} image_bytes={{.Size}}'
docker info --format 'engine={{.ServerVersion}} kernel={{.KernelVersion}} os={{.OperatingSystem}} arch={{.Architecture}} vm_cpus={{.NCPU}} vm_memory_bytes={{.MemTotal}} cgroup={{.CgroupVersion}} driver={{.Driver}}'
docker version --format 'client={{.Client.Version}} server={{.Server.Version}}'
uname -sm
curl --version | sed -n '1p'
shasum -a 256 Dockerfile Cargo.lock rust-toolchain.toml scripts/measure-resumable.sh scripts/measure-container-resources.sh
dd if=/dev/urandom of="$work/chunk" bs=1048576 count=8 2>/dev/null
for ((part=0; part<128; part++)); do dd if="$work/chunk" bs=1048576 2>/dev/null; done >"$work/object"
expected=$(shasum -a 256 "$work/object" | awk '{print $1}')
[[ $(wc -c <"$work/object" | tr -d ' ') == 1073741824 ]]
for round in 1 2 3 4 5; do
  phase=setup
  rm -f "$work/download-time"
  volume=$(docker volume create)
  address=$(perl -MIO::Socket::INET -e 'my $s=IO::Socket::INET->new(LocalAddr=>"127.0.0.1",LocalPort=>0,Listen=>1) or die $!; print "127.0.0.1:", $s->sockport')
  start_container "$address"
  ready=false
  for ((attempt=0; attempt<300; attempt++)); do
    if curl "${curl_args[@]}" --fail "$origin/_internal/ready" >/dev/null 2>&1; then ready=true; break; fi
    [[ $(docker inspect "$container" --format '{{.State.Running}}') == true ]]
    sleep 0.1
  done
  [[ $ready == true ]]
  docker inspect "$container" --format '{{json .HostConfig}}' | jq -ce '{Memory,MemorySwap,NanoCpus,PidsLimit,ReadonlyRootfs,Ulimits} | select(.Memory==134217728 and .MemorySwap==134217728 and .NanoCpus==1000000000 and .PidsLimit==64 and .ReadonlyRootfs==true)'
  observer=$(docker run -d --read-only --network none --pid "container:$container" --cgroupns host \
    --mount type=bind,src=/sys/fs/cgroup,dst=/sys/fs/cgroup,readonly \
    --mount "type=bind,src=$PWD/scripts/measure-container-resources.sh,dst=/measure.sh,readonly" \
    --volumes-from "$container:ro" --entrypoint bash "$observer_image" /measure.sh --observe)
  first=$(boundary)
  docker exec "$observer" stat -f -c 'data_filesystem=%T' /var/lib/gcs-mimic
  phase=idle
  phase_start
  sleep 60
  phase_end idle
  curl "${curl_args[@]}" --fail -H 'Content-Type: application/json' --data '{"name":"resource-bucket"}' "$origin/storage/v1/b?project=local-project" >/dev/null
  for mode in media resumable; do
    phase="$mode-init"
    rm -f "$work/download-time"
    : >"$work/latencies"
    if [[ $mode == resumable ]]; then
      status=$(curl "${curl_args[@]}" -X POST -H 'X-Upload-Content-Length: 1073741824' -H 'Content-Length: 0' \
        -D "$work/init" -o /dev/null -w '%{http_code}' "$origin/upload/storage/v1/b/resource-bucket/o?uploadType=resumable&name=object")
      [[ $status == 200 ]]
      location=$(awk 'tolower($1)=="location:" {gsub("\r", "", $2); print $2}' "$work/init")
      [[ $location == "$origin"/upload/storage/v1/* ]]
    fi
    phase="$mode-upload"
    phase_start
    if [[ $mode == media ]]; then
      metrics=$(curl "${curl_args[@]}" -X POST -T "$work/object" -H 'Expect:' -H 'Content-Type: application/octet-stream' \
        -o "$work/body" -w '%{http_code} %{time_total}' "$origin/upload/storage/v1/b/resource-bucket/o?uploadType=media&name=object")
      read -r status latency <<<"$metrics"
      [[ $status == 200 || $status == 201 ]]
      printf '%s\n' "$latency" >>"$work/latencies"
    else
      for ((offset=0; offset<1073741824; offset+=8388608)); do
        end=$((offset+8388607))
        metrics=$(curl "${curl_args[@]}" -X PUT -T "$work/chunk" -H 'Expect:' -H 'Content-Type: application/octet-stream' \
          -H "Content-Range: bytes $offset-$end/1073741824" -D "$work/headers" -o "$work/body" -w '%{http_code} %{time_total}' "$location")
        read -r status latency <<<"$metrics"
        if ((end+1 == 1073741824)); then [[ $status == 200 || $status == 201 ]]; else
          [[ $status == 308 ]]
          tr -d '\r' <"$work/headers" | grep -iq "^range: bytes=0-$end$"
        fi
        printf '%s\n' "$latency" >>"$work/latencies"
      done
    fi
    phase_end "$mode-upload" | jq -c '. + {bytes:1073741824,process_cpu_seconds_per_gib:.process_cpu_seconds,cgroup_cpu_seconds_per_gib:.cgroup_cpu_seconds,throughput_mib_s:(1024/.wall_seconds),errors:0}'
    jq -e '.size=="1073741824"' "$work/body" >/dev/null
    quantiles "$work/latencies"
    phase="$mode-download"
    phase_start
    actual=$(curl "${curl_args[@]}" --fail -w '%{stderr}http_status=%{http_code} request_seconds=%{time_total}\n' "$origin/storage/v1/b/resource-bucket/o/object?alt=media" \
      2>"$work/download-time" | shasum -a 256 | awk '{print $1}')
    phase_end "$mode-download" | jq -c '. + {bytes:1073741824,process_cpu_seconds_per_gib:.process_cpu_seconds,cgroup_cpu_seconds_per_gib:.cgroup_cpu_seconds,throughput_mib_s:(1024/.wall_seconds),errors:0}'
    [[ $actual == "$expected" ]]
    awk '/^http_status=200 request_seconds=[0-9.]+$/ {sub(/.*=/,""); print}' "$work/download-time" >"$work/download-latency"
    printf 'sha256 mode=%s expected=%s MATCH\n' "$mode" "$expected"
    quantiles "$work/download-latency"
    docker exec "$observer" du -sk /var/lib/gcs-mimic | awk '{print "phase_end_allocated_disk_KiB=" $1}'
    curl "${curl_args[@]}" --fail -X DELETE "$origin/storage/v1/b/resource-bucket/o/object" >/dev/null
  done
  curl "${curl_args[@]}" --fail "$origin/_internal/ready" >/dev/null
  phase=shutdown
  docker stop --time 10 "$container" >/dev/null
  docker inspect "$container" --format '{{json .State}}' | jq -ce '{Status,ExitCode,OOMKilled} | select(.Status=="exited" and .ExitCode==0 and .OOMKilled==false)'
  docker logs "$container" | jq -e 'select(.event=="stopped")' >/dev/null
  docker rm -f "$observer" >/dev/null
  observer=''
  docker rm "$container" >/dev/null
  container=''
  docker volume rm "$volume" >/dev/null
  volume=''
done
echo 'PASS: five isolated idle/media/resumable container measurements, exact 1 GiB metadata and SHA-256, no OOM and clean shutdown'
