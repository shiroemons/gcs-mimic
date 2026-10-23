#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$#" != 2 ] || [[ ! "$1" =~ ^https://(localhost|127\.0\.0\.1|\[::1\]):[0-9]+$ ]]; then
  echo 'Usage: bash scripts/tls-smoke.sh https://HOST:PORT CA_PEM' >&2
  exit 2
fi
endpoint=${1%/}
ca=$2
server="$(cargo metadata --locked --offline --no-deps --format-version 1 | jq -er .target_directory)/debug/gcs-mimic"
work=$(mktemp -d)
bucket="tls-smoke-$RANDOM-$RANDOM"
request() { curl --disable --noproxy '*' --silent --show-error --fail --max-time 15 --cacert "$ca" "$@"; }
cleanup() {
  result=$?
  if [ "$result" = 0 ]; then
    for name in media resumable; do
      request -X DELETE "$endpoint/storage/v1/b/$bucket/o/$name" >/dev/null
    done
    request -X DELETE "$endpoint/storage/v1/b/$bucket" >/dev/null
  else
    echo "Failed TLS smoke resources remain in $bucket for inspection" >&2
  fi
  rm -r "$work"
}
trap cleanup EXIT
request "$endpoint/_internal/ready" | jq -e '.status == "ok"' >/dev/null
request -H 'Content-Type: application/json' --data "{\"name\":\"$bucket\"}" \
  "$endpoint/storage/v1/b?project=tls-project" > "$work/bucket.json"
jq -e --arg base "$endpoint/" '.selfLink | startswith($base)' "$work/bucket.json" >/dev/null
printf 'Trusted TLS object bytes.\n' > "$work/input"
request --data-binary "@$work/input" \
  "$endpoint/upload/storage/v1/b/$bucket/o?uploadType=media&name=media" > "$work/object.json"
request "$endpoint/storage/v1/b/$bucket/o/media?alt=media" > "$work/output"
cmp "$work/input" "$work/output"
request -D "$work/headers" -H 'Content-Type: application/json' --data '{"name":"resumable"}' \
  "$endpoint/upload/storage/v1/b/$bucket/o?uploadType=resumable" >/dev/null
location=$(sed -n 's/^[Ll]ocation: //p' "$work/headers" | tr -d '\r')
[[ "$location" == "$endpoint/upload/"* ]]
request -X PUT -H 'Content-Range: bytes 0-2/3' --data-binary abc "$location" > "$work/resumable.json"
test "$(request "$endpoint/storage/v1/b/$bucket/o/resumable?alt=media")" = abc
public_url=$("$server" url public --base-url "$endpoint" --bucket "$bucket" --object media)
request "$public_url" > "$work/public"
cmp "$work/input" "$work/public"
echo 'PASS: trusted HTTPS probes, bucket/media/XML round trip and reachable resumable Location.'
