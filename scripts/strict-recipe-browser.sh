#!/usr/bin/env bash
# Runs real Chromium against the published main listener of examples/strict-local/compose.yaml.
# The browser runs in a throwaway container on the host network, so `localhost:PORT` is the same
# loopback address that the recipe publishes. Both subcommands below run inside that container.
set -euo pipefail

harness=/harness

if [ "${1:-}" = origin ] && [ "$#" = 2 ]; then
  exec node - "$2" > "$harness/origin.log" 2>&1 <<'NODE'
const http = require('node:http'), fs = require('node:fs');
const harness = '/harness', port = Number(process.argv[2]);
const html = fs.readFileSync(`${harness}/strict-recipe-browser.html`);
const manifest = fs.readFileSync(`${harness}/manifest.json`);
http.createServer((request, response) => {
  const isManifest = request.url === '/manifest.json';
  response.writeHead(200, {'Content-Type': isManifest ? 'application/json' : 'text/html; charset=utf-8', 'Cache-Control': 'no-store'});
  response.end(isManifest ? manifest : html);
}).listen(port, '127.0.0.1', () => fs.writeFileSync(`${harness}/origin.ready`, 'ready\n'));
NODE
fi

if [ "${1:-}" = visit ] && [ "$#" = 3 ]; then
  # Browser services try Google hosts even with these flags off, so name resolution outside
  # loopback is blocked here and the net log audit checks what was actually sent.
  args=(--headless --disable-gpu --no-sandbox --no-first-run --no-proxy-server
    --disable-background-networking --disable-component-update --disable-sync
    '--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1'
    "--log-net-log=$harness/netlog/$2.json" "--user-data-dir=$(mktemp -d)"
    --dump-dom --timeout=60000 --virtual-time-budget=30000)
  exec chromium "${args[@]}" "$3" 2> "$harness/browser-$2.log"
fi

if [ "$#" != 0 ]; then
  echo 'Usage: GCSM_IMAGE=gcs-mimic:local bash scripts/strict-recipe-browser.sh' >&2
  echo 'Needs Docker with Compose and jq; only pulling images and installing Chromium uses the network.' >&2
  exit 2
fi

cd "$(dirname "$0")/.."
export COMPOSE_FILE=examples/strict-local/compose.yaml
export GCSM_IMAGE=${GCSM_IMAGE:-gcs-mimic:local}
export GCSM_PORT=0
# Below the Linux ephemeral range, which Docker uses for the published port.
export FRONTEND_PORT=${FRONTEND_PORT:-$((20000 + RANDOM % 12000))}
browser_image=node:24.22.3-trixie-slim@sha256:af02d57b57719b9657a45199a79264a9437940cd55b3c7b3fc99615654f124a0
peer_image=curlimages/curl:8.23.0@sha256:3fb7e13e5d179a845c97d78204f3f1064d1907037f26a3ebf91710571151fd93
project=gcs-mimic-strict-browser-$$
container=$project-browser
work=$(mktemp -d)

cleanup() {
  local status=$?
  if [ "$status" != 0 ]; then
    docker exec "$container" sh -c "tail -n 40 $harness/origin.log $harness/browser-*.log" >&2 || true
    docker compose -p "$project" logs --no-color --tail 40 gcs >&2 || true
  fi
  docker rm --force "$container" >/dev/null 2>&1 || true
  docker compose -p "$project" down --volumes >/dev/null 2>&1 || true
  rm -r "$work"
  exit "$status"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# The application server's path into the recipe: credential-free, on the unpublished listener.
peer() {
  docker run --rm --read-only --network "${project}_default" "$peer_image" \
    --disable --noproxy '*' --silent --show-error --max-time 5 "$@"
}
sign() {
  docker compose -p "$project" exec -T gcs gcs-mimic url sign --expires 15m "$@"
}
sign_private() {
  sign --base-url "$api" --bucket app-private --object "$1" --method "$2" "${@:3}"
}
tamper() {
  case $1 in
    *X-Goog-Signature=0*) printf '%s' "${1/X-Goog-Signature=0/X-Goog-Signature=1}" ;;
    *) printf '%s' "$1" | sed -E 's/(X-Goog-Signature=)./\10/' ;;
  esac
}
seed() {
  peer --fail -H "Content-Type: $3" --data-binary "$4" \
    "http://gcs:4444/upload/storage/v1/b/$1/o?uploadType=media&name=$2" >/dev/null
}
in_browser() {
  docker exec "$container" bash "$harness/strict-recipe-browser.sh" "$@"
}
# A top-level navigation is how a user downloads: it needs no CORS, and the DOM shows the body.
navigate() {
  local name=$1 url=$2 expected=$3 absent=${4:-} dom
  dom=$(in_browser visit "$name" "$url") || fail "navigation $name did not complete"
  case $dom in *"$expected"*) ;; *) fail "navigation $name does not show $expected: $dom" ;; esac
  if [ -n "$absent" ]; then
    case $dom in *"$absent"*) fail "navigation $name reveals $absent: $dom" ;; esac
  fi
  echo "PASS: navigation $name shows $expected"
}
page() {
  local name=$1 url=$2 dom
  dom=$(in_browser visit "$name" "$url") || fail "page $name did not complete"
  printf '%s\n' "$dom" | sed -n '/<pre id="results">/,/<\/pre>/p' \
    | sed 's/^.*<pre id="results">//; s/<\/pre>.*$//'
  case $dom in
    *'<body data-status="pass">'*) ;;
    *) fail "page $name did not report success" ;;
  esac
}
# A fetch TypeError alone does not say why the request failed, so the net log must show the
# method the browser actually sent for that object name.
audit_sent() {
  local log=$1 object=$2 method=$3
  jq -e --arg object "$object" --arg method "$method" '
    .constants.logEventTypes.URL_REQUEST_START_JOB as $start
    | any(.events[]; .type == $start and (.params.url // "" | contains($object)) and .params.method == $method)' \
    "$work/netlog/$log.json" >/dev/null || fail "$log sent no $method for $object"
}
# The preflight was sent and the write was not: the browser refused it for CORS.
audit_preflight_only() {
  local log=$1 object=$2
  audit_sent "$log" "$object" OPTIONS
  jq -e --arg object "$object" '
    .constants.logEventTypes.URL_REQUEST_START_JOB as $start
    | [.events[] | select(.type == $start and (.params.url // "" | contains($object))) | .params.method]
    | all(. != "PUT")' "$work/netlog/$log.json" >/dev/null || fail "$log sent a PUT for $object after its preflight"
}

docker run --detach --name "$container" --network host "$browser_image" sleep infinity >/dev/null
docker exec "$container" mkdir -p /app/scripts "$harness/netlog"
docker cp -q scripts/install-chromium.sh "$container:/app/scripts/"
docker exec "$container" bash /app/scripts/install-chromium.sh >"$work/install-chromium.log" 2>&1 \
  || { tail -n 40 "$work/install-chromium.log" >&2; fail 'Chromium could not be installed in the browser container'; }
docker cp -q scripts/strict-recipe-browser.sh "$container:$harness/"
docker cp -q scripts/strict-recipe-browser.html "$container:$harness/"
docker exec "$container" chromium --version
docker exec "$container" node --version

docker compose -p "$project" config --quiet
docker compose -p "$project" up --detach --no-build --wait --wait-timeout 40
service=$(docker compose -p "$project" ps --quiet gcs)
address=$(docker compose -p "$project" port gcs 4443)
[[ $address == 127.0.0.1:* ]] || fail "the main listener is published on $address"
docker inspect "$service" | jq -e '.[0].HostConfig.PortBindings | keys == ["4443/tcp"]' >/dev/null \
  || fail 'the recipe publishes more than the main listener'
docker logs "$service" 2>&1 | jq -se 'any(.event == "ready" and .auth_mode == "strict" and .test_signer == "enabled")' >/dev/null \
  || fail 'the recipe is not in strict mode with the test signer'
port=${address##*:}
api=http://localhost:$port
frontend=http://localhost:$FRONTEND_PORT
checkout=$(git rev-parse HEAD)
echo "image under test: $GCSM_IMAGE $(docker image inspect "$GCSM_IMAGE" --format '{{.Id}}'), checkout $checkout"
echo 'PASS: recipe up, only 4443 published, strict mode, test signer enabled'

private_name=browser/private.txt
private_body='private bytes for the browser'
public_name=browser/public.txt
public_body='public bytes for the browser'
picture_name=browser/picture.svg
picture_type=image/svg+xml
picture_body='<svg xmlns="http://www.w3.org/2000/svg" width="3" height="2"></svg>'
seed app-private "$private_name" text/plain "$private_body"
seed app-public "$public_name" text/plain "$public_body"
seed app-private "$picture_name" "$picture_type" "$picture_body"
echo 'PASS: fixtures seeded through the unpublished trusted listener'

# The built-in test signer signs when neither --signer nor --private-key is given. Every URL is
# assigned first: a failing command substitution in an argument list would not stop the script.
type_header="content-type:$picture_type"
past=(--timestamp 20000101T000000Z)
download=$(sign_private "$private_name" GET)
download_tampered=$(tamper "$download")
download_expired=$(sign_private "$private_name" GET "${past[@]}")
picture=$(sign_private "$picture_name" GET)
picture_tampered=$(tamper "$picture")
picture_expired=$(sign_private "$picture_name" GET "${past[@]}")
upload=$(sign_private browser/uploaded.svg PUT --header "$type_header")
uploaded=$(sign_private browser/uploaded.svg GET)
upload_tampered=$(sign_private browser/tampered.svg PUT --header "$type_header")
upload_tampered=$(tamper "$upload_tampered")
upload_expired=$(sign_private browser/expired.svg PUT --header "$type_header" "${past[@]}")
upload_wrong_type=$(sign_private browser/wrong-type.svg PUT --header "$type_header")
upload_denied_header=$(sign_private browser/denied-header.svg PUT --header "$type_header")
upload_denied_origin=$(sign_private browser/denied-origin.svg PUT --header "$type_header")
portless=$(sign --base-url http://localhost --bucket app-private --object "$private_name" --method GET)
[[ $portless == http://localhost/* ]] || fail 'the portless URL is not signed for http://localhost'
portless=$api${portless#http://localhost}
other_host=$(sign --base-url "http://127.0.0.1:$port" --bucket app-private --object "$private_name" --method GET)
[[ $other_host == "http://127.0.0.1:$port/"* ]] || fail 'the other-host URL is not signed for 127.0.0.1'
other_host=$api${other_host#"http://127.0.0.1:$port"}
jq -n \
  --arg api "$api" --arg allowedOrigin "$frontend" \
  --arg privateName "$private_name" --arg privateBody "$private_body" \
  --arg publicName "$public_name" --arg publicBody "$public_body" \
  --arg pictureName "$picture_name" --arg pictureType "$picture_type" --arg pictureBody "$picture_body" \
  --arg download "$download" --arg downloadTampered "$download_tampered" --arg downloadExpired "$download_expired" \
  --arg picture "$picture" --arg pictureTampered "$picture_tampered" --arg pictureExpired "$picture_expired" \
  --arg upload "$upload" --arg uploaded "$uploaded" \
  --arg uploadTampered "$upload_tampered" --arg uploadExpired "$upload_expired" \
  --arg uploadWrongType "$upload_wrong_type" --arg uploadDeniedHeader "$upload_denied_header" \
  --arg uploadDeniedOrigin "$upload_denied_origin" \
  '{api: $api, allowedOrigin: $allowedOrigin,
    objects: {
      private: {name: $privateName, body: $privateBody},
      public: {name: $publicName, body: $publicBody, contentType: "text/plain"},
      picture: {name: $pictureName},
      anonymousUpload: "browser/anonymous.svg",
      anonymousPublicUpload: "browser/anonymous-public.txt"},
    picture: {contentType: $pictureType, body: $pictureBody, width: 3},
    urls: {download: $download, downloadTampered: $downloadTampered, downloadExpired: $downloadExpired,
      picture: $picture, pictureTampered: $pictureTampered, pictureExpired: $pictureExpired,
      upload: $upload, uploaded: $uploaded, uploadTampered: $uploadTampered, uploadExpired: $uploadExpired,
      uploadWrongType: $uploadWrongType, uploadDeniedHeader: $uploadDeniedHeader, uploadDeniedOrigin: $uploadDeniedOrigin}}' \
  > "$work/manifest.json"
# The negative checks mean nothing unless each URL is a real signed URL for the private bucket
# on the published listener, so a broken URL must fail here instead of being "refused" later.
jq -e --arg prefix "$api/app-private/" '
  .urls | length == 13 and all(to_entries[];
    (.value | startswith($prefix)) and (.value | contains("X-Goog-Algorithm=GOOG4-RSA-SHA256"))
    and (.value | test("X-Goog-Signature=[0-9a-f]+$|X-Goog-Signature=[0-9a-f]+&")))' \
  "$work/manifest.json" >/dev/null || fail 'the manifest holds a URL that is not signed for the published listener'
docker cp -q "$work/manifest.json" "$container:$harness/"
docker exec --detach "$container" bash "$harness/strict-recipe-browser.sh" origin "$FRONTEND_PORT"
for _ in {1..50}; do
  if docker exec "$container" test -f "$harness/origin.ready"; then break; fi
  sleep 0.1
done
docker exec "$container" test -f "$harness/origin.ready" || fail 'the page origin did not start'

navigate download "$download" "$private_body"
navigate download-portless "$portless" "$private_body"
navigate public "$api/app-public/$public_name" "$public_body"
navigate unsigned "$api/app-private/$private_name" AuthenticationRequired "$private_body"
navigate list-xml "$api/app-private" AuthenticationRequired "$private_name"
navigate list-json "$api/storage/v1/b/app-private/o" Anonymous "$private_name"
navigate tampered "$download_tampered" SignatureDoesNotMatch "$private_body"
navigate expired "$download_expired" ExpiredToken "$private_body"
navigate other-host "$other_host" SignatureDoesNotMatch "$private_body"
echo 'PASS: scenario navigation'

page stock-allowed "$frontend/?role=allowed&phase=stock"
echo 'PASS: scenario page-stock'
page stock-denied "http://127.0.0.1:$FRONTEND_PORT/?role=denied&phase=stock"
echo 'PASS: scenario page-denied-origin'

# What the browser wrote and what it was refused, seen from the application server's side.
[[ $(peer --fail "http://gcs:4444/storage/v1/b/app-private/o/browser%2Fuploaded.svg?alt=media") == "$picture_body" ]] \
  || fail 'the signed upload did not store the exact bytes'
peer --fail 'http://gcs:4444/storage/v1/b/app-private/o/browser%2Fuploaded.svg?projection=full' \
  | jq -e '.contentType == "image/svg+xml" and .owner.entity == "user-test-signer@gcs-mimic.invalid"' >/dev/null \
  || fail 'the signed upload did not record its signed Content-Type and the test signer as owner'
peer --fail 'http://gcs:4444/storage/v1/b/app-private/o' \
  | jq -e '[.items[].name] == ["browser/picture.svg", "browser/private.txt", "browser/uploaded.svg"]' >/dev/null \
  || fail 'a refused or blocked upload published an object'
peer --fail 'http://gcs:4444/storage/v1/b/app-public/o' \
  | jq -e '[.items[].name] == ["browser/public.txt"]' >/dev/null \
  || fail 'a refused browser write reached the public bucket'
# The page expects ETag to stay hidden, which only means something if the server sends it.
curl --disable --noproxy '*' --fail --silent --show-error --max-time 5 --head "http://$address/app-public/$public_name" \
  | tr -d '\r' | grep -i '^etag:' >/dev/null || fail 'the public object has no ETag response header'
echo 'PASS: the signed upload stored exact bytes owned by the test signer; every refused or blocked write published nothing'

# The README's own suggestion for pages that fetch() signed downloads, applied to the live bucket.
cors_with_get=$(jq -n --arg origin "$frontend" \
  '{cors: [{origin: [$origin], method: ["PUT", "GET"], responseHeader: ["Content-Type"], maxAgeSeconds: 3600}]}')
peer --fail -X PATCH -H 'Content-Type: application/json' --data-binary "$cors_with_get" \
  'http://gcs:4444/storage/v1/b/app-private' >/dev/null
page get-added "$frontend/?role=allowed&phase=get-added"
echo 'PASS: scenario page-get-added'

docker cp -q "$container:$harness/netlog" "$work/netlog"
audit_preflight_only stock-allowed /browser/anonymous-public.txt
audit_preflight_only stock-allowed /browser/denied-header.svg
audit_preflight_only stock-denied /browser/denied-origin.svg
audit_sent stock-allowed /browser/private.txt GET
audit_sent stock-denied /browser/public.txt GET
echo 'PASS: every blocked upload sent its preflight and no PUT; blocked reads reached the server'
for log in "$work"/netlog/*.json; do
  jq -e --arg main "$port" --arg page "$FRONTEND_PORT" '
    .constants.logEventTypes as $types
    | [.events[] | select(.type == $types.TCP_CONNECT_ATTEMPT and .params.address != null) | .params.address] as $addresses
    | ($addresses | length) > 0
      and all($addresses[]; . == ("127.0.0.1:" + $main) or . == ("[::1]:" + $main)
                         or . == ("127.0.0.1:" + $page) or . == ("[::1]:" + $page))
      and ([.events[] | select(.type == $types.UDP_BYTES_SENT)] | length) == 0' \
    "$log" >/dev/null || fail "the browser connected elsewhere or sent UDP in $(basename "$log")"
done
echo "PASS: scenario network-audit: TCP connections in $(find "$work/netlog" -name '*.json' | wc -l | tr -d ' ') net logs went only to the published listener and the page origin on loopback, no UDP datagrams were sent, and name resolution outside loopback is blocked"
echo 'PASS: strict local recipe browser check completed'
