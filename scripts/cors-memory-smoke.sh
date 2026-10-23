#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || $1 == --help ]]; then
  echo 'Usage: bash scripts/cors-memory-smoke.sh IMAGE' >&2
  exit 2
fi
cd "$(dirname "$0")/.."
image=$1
work=$(mktemp -d)
container=
volume="gcs-mimic-cors-memory-$$"
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "$container" ]]; then
    docker inspect --format 'final_state={{json .State}} memory={{.HostConfig.Memory}} memorySwap={{.HostConfig.MemorySwap}} image={{.Image}}' "$container"
    if [[ $status != 0 ]]; then docker logs "$container"; fi
    docker rm --force "$container" >/dev/null
  fi
  docker volume rm "$volume" >/dev/null
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

request() {
  curl --disable --noproxy '*' --fail --silent --show-error --max-time 20 "$@"
}
ready() {
  local attempt
  for ((attempt = 0; attempt < 40; attempt++)); do
    if docker exec "$container" gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready >/dev/null 2>&1; then
      address=$(docker port "$container" 4443)
      [[ $address == 127.0.0.1:* ]]
      return
    fi
    sleep 1
  done
  echo 'Readiness timed out' >&2
  return 1
}
memory_state() {
  local name
  docker inspect --format 'running={{.State.Running}} OOMKilled={{.State.OOMKilled}} exit={{.State.ExitCode}} memory={{.HostConfig.Memory}} memorySwap={{.HostConfig.MemorySwap}} readonly={{.HostConfig.ReadonlyRootfs}} user={{.Config.User}}' "$container"
  [[ $(docker inspect --format '{{.State.Running}} {{.State.OOMKilled}} {{.HostConfig.Memory}} {{.HostConfig.MemorySwap}} {{.HostConfig.ReadonlyRootfs}} {{.Config.User}}' "$container") == 'true false 134217728 134217728 true 10001:10001' ]]
  for name in memory.current memory.peak; do
    if docker cp "$container:/sys/fs/cgroup/$name" "$work/$name" 2>"$work/cgroup-error" && [[ $(<"$work/$name") =~ ^[0-9]+$ ]]; then
      printf '%s=%s bytes (Linux cgroup)\n' "$name" "$(<"$work/$name")"
    else
      printf '%s=unavailable via Docker archive API; no cgroup peak inferred\n' "$name"
    fi
  done
  docker stats --no-stream --format 'Docker stats snapshot: {{.MemUsage}} (not process RSS or peak)' "$container"
}
traverse() {
  local mode=$1 maximum=$2 prefix=$3 token= pages=0 count
  local fields='items(name),nextPageToken'
  if [[ $mode == full ]]; then fields='items,nextPageToken'; fi
  : >"$work/actual-names"
  : >"$work/tokens"
  while :; do
    request --get --data-urlencode project=cors-memory --data-urlencode "prefix=$prefix" \
      --data-urlencode "maxResults=$maximum" --data-urlencode "fields=$fields" \
      --data-urlencode "pageToken=$token" "http://$address/storage/v1/b" >"$work/page.json"
    jq -e --argjson maximum "$maximum" '.items | type == "array" and length > 0 and length <= $maximum' "$work/page.json" >/dev/null
    if [[ $mode == full ]]; then
      jq -e 'all(.items[]; (.cors | type == "array" and length == 20000) and all(.cors[]; . == {}))' "$work/page.json" >/dev/null
    fi
    jq -r '.items[].name' "$work/page.json" >>"$work/actual-names"
    pages=$((pages + 1))
    [[ $pages -le 60 ]]
    token=$(jq -er '.nextPageToken // "" | strings' "$work/page.json")
    if [[ -z $token ]]; then break; fi
    if grep -Fxq "$token" "$work/tokens"; then echo 'Repeated page token' >&2; return 1; fi
    printf '%s\n' "$token" >>"$work/tokens"
  done
  grep -F "$prefix" "$work/expected-names" >"$work/selected-names"
  cmp "$work/selected-names" "$work/actual-names"
  count=$(wc -l <"$work/actual-names" | tr -d ' ')
  echo "PASS: $phase $mode traversal maxResults=$maximum prefix=$prefix buckets=$count pages=$pages; exact ordered names, no missing/duplicate buckets or truncated rules"
}
basic_reads() {
  request "http://$address/_internal/health" >/dev/null
  request "http://$address/storage/v1/b/memory-cors-001" \
    | jq -e '.name == "memory-cors-001" and (.cors | length == 20000) and all(.cors[]; . == {})' >/dev/null
  request "http://$address/storage/v1/b/memory-cors-001/o/probe.txt?alt=media" >"$work/actual-bytes"
  cmp "$work/expected-bytes" "$work/actual-bytes"
  request --get --data-urlencode 'fields=items(name,size)' \
    "http://$address/storage/v1/b/memory-cors-001/o" \
    | jq -e '.items == [{name:"probe.txt",size:"18"}]' >/dev/null
  echo "PASS: $phase basic health, bucket GET, object media GET and selected-field object list"
}

docker image inspect --format 'image={{.Id}} platform={{.Os}}/{{.Architecture}} user={{.Config.User}}' "$image"
[[ $(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image") == linux/arm64 ]]
docker volume create "$volume" >/dev/null
container=$(docker run --pull never --detach --read-only --no-healthcheck --cap-drop ALL \
  --security-opt no-new-privileges --memory 128m --memory-swap 128m --cpus 1 --pids-limit 64 \
  --ulimit nofile=1024:1024 --mount "type=volume,src=$volume,dst=/var/lib/gcs-mimic" \
  -p 127.0.0.1::4443 "$image" serve --scheme http --listen 0.0.0.0:4443 \
  --data-dir /var/lib/gcs-mimic --api-base-url http://127.0.0.1:4443)
ready
printf 'cors-memory-smoke\n' >"$work/expected-bytes"
: >"$work/expected-names"
for ((i = 1; i <= 60; i++)); do
  printf -v name 'memory-cors-%03d' "$i"
  printf '%s\n' "$name" >>"$work/expected-names"
  jq -cn --arg name "$name" '{name:$name,cors:[range(0;20000)|{}]}' >"$work/input.json"
  [[ $(wc -c <"$work/input.json") -lt 65536 ]]
  request -H 'Content-Type: application/json' --data-binary @"$work/input.json" \
    'http://'"$address"'/storage/v1/b?project=cors-memory' \
    | jq -e --arg name "$name" '.name == $name and (.cors | length == 20000) and all(.cors[]; . == {})' >/dev/null
done
echo 'PASS: accepted 60 configurations, each 20000 empty rules / 60001 minified CORS bytes; each request below 64 KiB'
request -H 'Content-Type: text/plain' --data-binary @"$work/expected-bytes" \
  "http://$address/upload/storage/v1/b/memory-cors-001/o?uploadType=media&name=probe.txt" >/dev/null
for phase in initial restart; do
  if [[ $phase == restart ]]; then
    docker stop --timeout 15 "$container" >/dev/null
    [[ $(docker inspect --format '{{.State.ExitCode}} {{.State.OOMKilled}}' "$container") == '0 false' ]]
    docker start "$container" >/dev/null
    ready
  fi
  status=$(request --get --data-urlencode project=cors-memory --data-urlencode 'fields=items(name)' \
    -o "$work/prior-fields.json" -w '%{http_code}' "http://$address/storage/v1/b")
  [[ $status == 200 ]]
  jq -e '.items | type == "array" and length > 0' "$work/prior-fields.json" >/dev/null
  echo "PASS: $phase exact former OOM selector fields=items(name) HTTP $status"
  traverse names 1000 memory-cors-
  traverse full 1000 memory-cors-
  traverse full 7 memory-cors-0
  traverse full 1 memory-cors-00
  basic_reads
  memory_state
done
echo 'PASS: cors-memory-smoke; sequential accepted-data resource regression only, no throughput or concurrency claim'
