#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
repository=${GCS_MIMIC_REFERENCE_REPO:?Set GCS_MIMIC_REFERENCE_REPO to the pinned repository URL listed in the README comparison sources}
revision=896ece51e49eed0946089f3089f6343855c0d106
archive_sha256=13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc
go_toolchain=$(awk '$1 == "toolchain" {print $2}' sdk-tests/go/go.mod)
reference_only=${GCS_MIMIC_REFERENCE_ONLY:-0}
[[ $reference_only == 0 || $reference_only == 1 ]]
for command in curl jq go cargo tar shasum git nc; do
  if ! command -v "$command" >/dev/null; then echo "Missing command: $command" >&2; exit 1; fi
done
[[ $go_toolchain =~ ^go[0-9]+\.[0-9]+\.[0-9]+$ ]]
work=$(mktemp -d -t gcs-mimic-compare.XXXXXX)
pids=()
targets=()
origins=()
cleanup() {
  status=$?
  trap - EXIT
  for pid in ${pids[@]+"${pids[@]}"}; do
    if kill -0 "$pid" 2>/dev/null; then kill -TERM "$pid" || status=1; fi
    # The reference CLI exits on SIGTERM; native shutdown exits successfully.
    if wait "$pid"; then :; else
      server_status=$?
      if [[ $server_status -ne 143 ]]; then status=1; fi
    fi
  done
  if [[ $status -ne 0 ]]; then
    for log in "$work"/*.log; do
      [[ -f $log ]] || continue
      printf '\n--- %s ---\n' "${log##*/}" >&2
      sed -n '1,100p' "$log" >&2
    done
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo 'Building selected comparison targets in a temporary directory' >&2
curl --fail --location --silent --show-error --connect-timeout 20 --max-time 180 \
  "${repository%.git}/archive/$revision.tar.gz" -o "$work/source.tar.gz"
actual_sha256=$(shasum -a 256 "$work/source.tar.gz")
[[ ${actual_sha256%% *} == "$archive_sha256" ]]
mkdir "$work/reference-source"
tar -xzf "$work/source.tar.gz" --strip-components=1 -C "$work/reference-source"
export GOTOOLCHAIN="$go_toolchain"
# Keep build artifacts outside the checkout; module downloads retain Go checksum verification.
export GOCACHE="$work/go-build"
(
  cd "$work/reference-source"
  go build -mod=readonly -o "$work/reference-emulator" .
)
(
  cd sdk-tests/go
  go build -mod=readonly -o "$work/free-port" ./cmd/free-port
)
server="$work/rust-target/debug/gcs-mimic"
if [[ $reference_only == 0 ]]; then
  env -u RUSTUP_TOOLCHAIN CARGO_TARGET_DIR="$work/rust-target" cargo build --locked --offline
fi

start_target() {
  local target=$1 address origin pid ready=false
  shift
  address=$("$work/free-port")
  [[ $address =~ ^127\.0\.0\.1:[0-9]+$ ]]
  origin="http://$address"
  if [[ $target == reference-* ]]; then
    # Empty environment prevents inherited event hooks or seeding from contacting other services.
    env -i PATH="$PATH" "$work/reference-emulator" -scheme http -host 127.0.0.1 \
      -port "${address##*:}" -external-url "$origin" -backend "$1" \
      -filesystem-root "$work/$target-data" -data "${3:-$work/no-seed}" \
      -public-host "${2:-storage.googleapis.com}" \
      >"$work/$target.log" 2>&1 &
  else
    env -i PATH="$PATH" "$server" serve --scheme http --listen "$address" --api-base-url "$origin" \
      --data-dir "$work/$target-data" "$@" >"$work/$target.log" 2>&1 &
  fi
  pid=$!
  pids+=("$pid")
  for ((attempt = 0; attempt < 100; attempt++)); do
    if ! kill -0 "$pid" 2>/dev/null; then echo "Server exited before readiness: $target" >&2; exit 1; fi
    if curl --noproxy '*' --proxy '' --fail --silent --output /dev/null --max-time 1 \
      "$origin/storage/v1/b?project=compare-project"; then ready=true; break; fi
    sleep 0.1
  done
  if [[ $ready != true ]]; then echo "Server readiness timed out: $target" >&2; exit 1; fi
  targets+=("$target")
  origins+=("$origin")
}

stop_last_target() {
  local index=$((${#pids[@]} - 1)) pid server_status
  pid=${pids[$index]}
  kill -TERM "$pid"
  if wait "$pid"; then :; else
    server_status=$?
    [[ $server_status == 143 ]] || return "$server_status"
  fi
  unset 'pids[index]'
}

printf 'pinned reference emulator v1.56.1 commit %s\narchive SHA256 %s\n' "$revision" "$archive_sha256"
go version
printf 'checkout source commit %s (reference-only=%s)\n' "$(git rev-parse HEAD)" "$reference_only"
printf 'comparison script blob %s\n' "$(git hash-object scripts/compare-reference-emulator.sh)"
printf '%s\n' \
  'Raw records preserve status, selected header values, and the exact response body as a JSON string.' \
  'Selected headers: content-type, content-length, etag, location, x-goog-*, www-authenticate.' \
  'Comparison only: JSON object keys/whitespace are canonicalized; arrays retain order.' \
  'Comparison only: nonempty etag fields/headers become <etag>; RFC3339 timeCreated/updated/timeDeleted become <timestamp>.' \
  'Comparison only: this target loopback origin becomes <origin>; paths, schemes and other hosts remain unchanged.' \
  'No fields are added or removed. Empty bodies, JSON null, errors, defaults and object generations remain distinct.' \
  'Request content-type is application/json except the text/plain media upload; no credentials are sent.'

start_target reference-filesystem filesystem
start_target reference-memory memory
if [[ $reference_only == 0 ]]; then
  help=$("$server" serve --help)
  profile_args=()
  if [[ $help == *--compat-profile* ]]; then profile_args=(--compat-profile native); fi
  start_target mimic-native ${profile_args[@]+"${profile_args[@]}"}
  if [[ $help == *--compat-profile* ]]; then
    start_target mimic-legacy-emulator --compat-profile legacy-emulator
  else
    echo 'SKIPPED mimic-legacy-emulator: binary does not support --compat-profile'
  fi
fi

request() {
  local label=$1 method=$2 path=$3 body=${4-} content_type=${5:-application/json} condition=${6-}
  local index target origin status result
  local args=()
  if [[ $method == POST || $method == PATCH || $method == PUT ]]; then args=(--data-binary "$body" --header "Content-Type: $content_type"); fi
  if [[ -n $condition ]]; then args+=(--header "$condition"); fi
  for index in "${!targets[@]}"; do
    target=${targets[$index]}
    origin=${origins[$index]}
    : >"$work/body"
    status=$(curl --noproxy '*' --proxy '' --globoff --path-as-is --silent --show-error \
      --connect-timeout 2 --max-time 15 --request "$method" ${args[@]+"${args[@]}"} \
      --dump-header "$work/headers" --output "$work/body" --write-out '%{http_code}' "$origin$path")
    [[ $status =~ ^[1-5][0-9][0-9]$ ]]
    result=$(jq -nSc --arg target "$target" --arg label "$label" --arg method "$method" \
      --arg path "$path" --arg request_body "$body" --arg condition "$condition" --arg origin "$origin" --argjson status "$status" \
      --rawfile headers "$work/headers" --rawfile body "$work/body" '
      def normalize:
        walk(if type == "object" then with_entries(
          if (.key == "etag" and (.value | type) == "string" and .value != "") then .value = "<etag>"
          elif ((.key == "timeCreated" or .key == "updated" or .key == "timeDeleted")
            and (.value | type) == "string"
            and (.value | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+(Z|[+-][0-9:]+)$")))
            then .value = "<timestamp>" else . end)
          elif type == "string" then split($origin) | join("<origin>") else . end);
      ($headers | split("\n") | map(
        try capture("^(?<key>[^:]+):[ ]*(?<value>.*)$") catch empty)
        | map(.key |= ascii_downcase | .value |= rtrimstr("\r"))
        | map(select(.key == "content-type" or .key == "content-length" or .key == "etag"
          or .key == "location" or .key == "www-authenticate" or (.key | startswith("x-goog-"))))) as $selected
      | {target:$target, case:$label, request:({method:$method,path:$path,body:$request_body}
          + if $condition == "" then {} else {headers:[$condition]} end),
        status:$status, headers:$selected, body:$body,
        comparison:{status:$status,headers:($selected | map(
          if .key == "etag" and .value != "" then .value = "<etag>" else . end) | normalize),body:
          (try {format:"json",value:($body | fromjson | normalize)}
           catch {format:"text",value:($body | normalize)})}}')
    printf 'RAW %s\n' "$result"
    printf '%s\n' "$result" >>"$work/results.jsonl"
  done
}

state() {
  local label=$1 name=$2 encoded
  encoded=$(jq -rn --arg name "$name" '$name | @uri')
  request "$label-get-state" GET "/storage/v1/b/$encoded"
  request "$label-list-state" GET "/storage/v1/b?project=compare-project&prefix=$encoded"
}

create() {
  local label=$1 name=$2 fields=${3:-'{}'} body
  body=$(jq -cn --arg name "$name" --argjson fields "$fields" '{name:$name} + $fields')
  request "$label" POST '/storage/v1/b?project=compare-project' "$body"
  state "$label" "$name"
}

request initial-list GET '/storage/v1/b?project=compare-project'
request name-only-no-project POST /storage/v1/b '{"name":"compare-no-project"}'
state name-only-no-project compare-no-project
create name-only compare-basic
request get-existing GET /storage/v1/b/compare-basic
request duplicate-create POST '/storage/v1/b?project=compare-project' '{"name":"compare-basic"}'
state duplicate-create compare-basic
create versioning-enabled compare-versioned '{"versioning":{"enabled":true}}'
create versioning-disabled compare-unversioned '{"versioning":{"enabled":false}}'
create default-hold-enabled compare-held '{"defaultEventBasedHold":true}'
create default-hold-disabled compare-unheld '{"defaultEventBasedHold":false}'
create both-settings compare-both '{"versioning":{"enabled":true},"defaultEventBasedHold":true}'
# These names follow the pinned source regex but violate the native validator.
for name in aa goog-compare google-compare 192.168.0.1 a..b aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; do
  create "source-valid-$name" "$name"
done

# Keep reference routing observations separate from native conformance assertions.
reference_host_cases() {
local host_index=0
host_seed="$work/host-seed"
for bucket in probe-bucket dotted.probe-bucket fallback.invalid \
  storage.example.invalid probe-bucket.storage.example.invalid \
  dotted.probe-bucket.storage.example.invalid probe-bucket.deep.storage.example.invalid \
  storage.example.invalid. storage.example.invalid.evil \
  127.0.0.1 '[::1]' '[::1]:8443'; do
  mkdir -p "$host_seed/$bucket/probe-bucket" "$host_seed/$bucket/storage/v1"
  printf 'bucket=%s;object=object' "$bucket" >"$host_seed/$bucket/object"
  printf 'bucket=%s;object=probe-bucket/object' "$bucket" >"$host_seed/$bucket/probe-bucket/object"
  printf 'bucket=%s;object=storage/v1/b' "$bucket" >"$host_seed/$bucket/storage/v1/b"
done

host_request() {
  local label=$1 host=$2 path=$3 method=${4:-GET} target=${5:-$3} status
  local args=()
  if [[ $method == PUT ]]; then args=(--data-binary host-put); fi
  if [[ $method == HEAD ]]; then
    # Inspect wire bytes so an invalid HEAD body cannot be hidden by curl's HEAD handling.
    printf 'HEAD %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n' "$target" "$host" \
      | nc -w 2 127.0.0.1 "${host_origin##*:}" >"$work/host-head-wire"
    jq -ejrRs 'index("\r\n\r\n") as $end | if $end == null then error("missing HEAD headers") else .[:$end] end' \
      "$work/host-head-wire" >"$work/host-headers"
    jq -jrRs 'index("\r\n\r\n") as $end | .[$end + 4:]' "$work/host-head-wire" >"$work/host-body"
    status=$(awk 'NR == 1 {print $2}' "$work/host-headers")
  else
    status=$(curl --noproxy '*' --proxy '' --globoff --path-as-is --silent --show-error \
      --connect-timeout 2 --max-time 15 --request "$method" --request-target "$target" \
      --header "Host: $host" ${args[@]+"${args[@]}"} --dump-header "$work/host-headers" \
      --output "$work/host-body" --write-out '%{http_code}' "$host_origin$path")
  fi
  jq -ncS --arg case "$label" --arg public_host "$public_host" --arg host "$host" \
    --arg target "$target" --arg method "$method" --argjson status "$status" \
    --rawfile headers "$work/host-headers" --rawfile body "$work/host-body" \
    '{case:$case,public_host:$public_host,request:{method:$method,host:$host,target:$target},
      status:$status,headers:($headers | split("\n") | map(select(test("^(Content-Type|Content-Length|Location):";"i")) | rtrimstr("\r"))),body:$body}' \
    | tee -a "$work/host-results.jsonl" | sed 's/^/HOST /'
}

for public_host in storage.example.invalid storage.example.invalid:80 storage.example.invalid:8443 \
  127.0.0.1 '[::1]' '[::1]:8443'; do
  host_index=$((host_index + 1))
  start_target "reference-host-$host_index" memory "$public_host" "$host_seed"
  host_origin=${origins[${#origins[@]}-1]}
  for request_host in storage.example.invalid storage.example.invalid:80 \
    storage.example.invalid:443 storage.example.invalid:8443 Storage.Example.Invalid \
    storage.example.invalid. storage.example.invalid.evil 127.0.0.1 127.0.0.1:8443 \
    '[::1]' '[::1]:8443'; do
    host_request authority "$request_host" /probe-bucket/object
  done
  host_request public-root "$public_host" /probe-bucket
  host_request virtual-simple "probe-bucket.$public_host" /object
  host_request virtual-portless probe-bucket.storage.example.invalid /object
  host_request virtual-port-80 probe-bucket.storage.example.invalid:80 /object
  host_request virtual-port-8443 probe-bucket.storage.example.invalid:8443 /object
  host_request virtual-case Probe-Bucket.Storage.Example.Invalid /object
  host_request virtual-trailing-dot probe-bucket.storage.example.invalid. /object
  host_request virtual-dotted "dotted.probe-bucket.$public_host" /object
  host_request overlapping-suffix "probe-bucket.deep.$public_host" /object
  host_request virtual-root "probe-bucket.$public_host" /
  host_request json-precedence "probe-bucket.$public_host" '/storage/v1/b?project=compare-project'
  host_request arbitrary-fallback fallback.invalid /object
  host_request arbitrary-root fallback.invalid /
  host_request missing-bucket "$public_host" /missing-bucket/object
  host_request missing-object "$public_host" /probe-bucket/missing-object
  host_request head "$public_host" /probe-bucket/object HEAD
  host_request put "$public_host" '/probe-bucket/new-object?X-Goog-Algorithm=probe' PUT
  host_request put-read "$public_host" /probe-bucket/new-object
  host_request absolute-target fallback.invalid /probe-bucket/object GET "http://$public_host/probe-bucket/object"
  host_request absolute-conflict "$public_host" /object GET 'http://fallback.invalid/object'
  if [[ $host_index != 6 ]]; then stop_last_target; fi
done

for raw_case in duplicate-host malformed-host missing-host; do
  case $raw_case in
    duplicate-host) host_lines=$'Host: storage.example.invalid\r\nHost: fallback.invalid' ;;
    malformed-host) host_lines='Host: bad host' ;;
    missing-host) host_lines='' ;;
  esac
  printf 'GET /object HTTP/1.1\r\n%s\r\nConnection: close\r\n\r\n' "$host_lines" \
    | nc -w 2 127.0.0.1 "${host_origin##*:}" >"$work/host-raw"
  jq -ncS --arg case "$raw_case" --arg public_host "$public_host" --arg headers "$host_lines" \
    --rawfile response "$work/host-raw" \
    '{case:$case,public_host:$public_host,request:{method:"GET",target:"/object",headers:$headers},response:$response}' \
    | tee -a "$work/host-results.jsonl" | sed 's/^/HOST /'
done
stop_last_target
jq -es '
  def public_match($public; $host):
    $host == $public or (($public | contains(":" ) | not) and ($host | startswith($public + ":")));
  def marker($bucket; $object): .status == 200 and .body == ("bucket=" + $bucket + ";object=" + $object);
  def not_found: .status == 404 and .body == "Not Found\n";
  def fallback($host; $object):
    ($host | split(":")[0]) as $bucket
    | if (["storage.example.invalid", "storage.example.invalid.", "storage.example.invalid.evil",
           "probe-bucket.storage.example.invalid", "dotted.probe-bucket.storage.example.invalid",
           "probe-bucket.deep.storage.example.invalid", "127.0.0.1", "fallback.invalid"] | index($bucket)) != null
      then marker($bucket; $object) else not_found end;
  ["public-root", "virtual-simple", "virtual-portless", "virtual-port-80", "virtual-port-8443",
   "virtual-case", "virtual-trailing-dot", "virtual-dotted", "overlapping-suffix", "virtual-root",
   "json-precedence", "arbitrary-fallback", "arbitrary-root", "missing-bucket", "missing-object",
   "head", "put", "put-read", "absolute-target", "absolute-conflict"] as $cases
  | length == 189
  and (map(select(has("status"))) | group_by(.public_host) | length == 6 and all(
    length == 31 and (map(.case) | sort) == (($cases + [range(11) | "authority"]) | sort)))
  and (map(select(.case == "authority")) | group_by(.public_host) | all(
    (map(.request.host) | sort) == (["storage.example.invalid", "storage.example.invalid:80",
      "storage.example.invalid:443", "storage.example.invalid:8443", "Storage.Example.Invalid",
      "storage.example.invalid.", "storage.example.invalid.evil", "127.0.0.1", "127.0.0.1:8443",
      "[::1]", "[::1]:8443"] | sort)))
  and all(.[];
    if .case == "authority" then
      if public_match(.public_host; .request.host) then marker("probe-bucket"; "object")
      else fallback(.request.host; "probe-bucket/object") end
    elif .case == "virtual-simple" or .case == "absolute-target" then marker("probe-bucket"; "object")
    elif (.case == "virtual-portless" or .case == "virtual-port-80" or .case == "virtual-port-8443") then
      if public_match(.public_host; (.request.host | ltrimstr("probe-bucket.")))
      then marker("probe-bucket"; "object") else marker("probe-bucket.storage.example.invalid"; "object") end
    elif .case == "virtual-dotted" or .case == "overlapping-suffix" then fallback(.request.host; "object")
    elif .case == "virtual-case" or .case == "virtual-trailing-dot" or .case == "missing-bucket"
      or .case == "missing-object" then not_found
    elif .case == "public-root" or .case == "virtual-root" then
      .status == 200 and (.headers | index("Content-Type: application/xml")) != null
      and (.body | contains("<ListBucketResult><Name>probe-bucket</Name>") and contains("<Key>object</Key>"))
    elif .case == "json-precedence" then
      .status == 200 and (.body | fromjson | .kind == "storage#buckets" and (.items | any(.name == "probe-bucket")))
    elif .case == "arbitrary-fallback" or .case == "absolute-conflict" then marker("fallback.invalid"; "object")
    elif .case == "arbitrary-root" then .status == 404 and .body == "404 page not found\n"
    elif .case == "head" then .status == 200 and .body == "" and (.headers | index("Content-Length: 33")) != null
    elif .case == "put" then
      .status == 200 and (.body | fromjson | .bucket == "probe-bucket" and .name == "new-object" and .size == "8")
    elif .case == "put-read" then .status == 200 and .body == "host-put"
    elif .case == "duplicate-host" or .case == "malformed-host" or .case == "missing-host"
      then .response | startswith("HTTP/1.1 400 ")
    else error("unasserted Host case: " + .case) end)
  and (map(select(has("response")) | .case) | sort) == ["duplicate-host", "malformed-host", "missing-host"]
' "$work/host-results.jsonl" >/dev/null
echo 'HOST-ASSERT all 189 records: authority/ports/IPv6, virtual/fallback/DNS, JSON, absolute targets, roots, HEAD, errors and PUT: PASS'
}
for name in a Uppercase -bad bad- 'bad!' ''; do create "source-invalid-$name" "$name"; done
request malformed-json POST '/storage/v1/b?project=compare-project' '{"name":"compare-malformed",'
state malformed-json compare-malformed
request missing-name POST '/storage/v1/b?project=compare-project' '{}'
state missing-name ''
request get-missing GET /storage/v1/b/compare-missing
state get-missing compare-missing
request delete-empty DELETE /storage/v1/b/compare-basic
state delete-empty compare-basic
request delete-repeat DELETE /storage/v1/b/compare-basic
state delete-repeat compare-basic
request delete-missing DELETE /storage/v1/b/compare-missing
state delete-missing compare-missing
create delete-recreate compare-basic
create nonempty-create compare-nonempty
request nonempty-upload POST '/upload/storage/v1/b/compare-nonempty/o?uploadType=media&name=kept.txt' 'preserved payload' text/plain
request delete-nonempty DELETE /storage/v1/b/compare-nonempty
state delete-nonempty compare-nonempty
request nonempty-objects GET /storage/v1/b/compare-nonempty/o
request nonempty-object-bytes GET '/storage/v1/b/compare-nonempty/o/kept.txt?alt=media'

patch_bucket() {
  local label=$1 body=$2 query=${3:-} name=${4:-compare-patch}
  request "$label" PATCH "/storage/v1/b/$name$query" "$body"
  state "$label" "$name"
}

create patch-create compare-patch
patch_bucket patch-label-add '{"labels":{"keep":"one","remove":"two"}}'
patch_bucket patch-label-merge '{"labels":{"add":"three"}}'
patch_bucket patch-label-remove '{"labels":{"remove":null}}'
patch_bucket patch-label-empty '{"labels":{}}'
patch_bucket patch-label-null '{"labels":null}'
patch_bucket patch-label-restore '{"labels":{"keep":"one"}}'
patch_bucket patch-label-empty-value '{"labels":{"empty":""}}'
patch_bucket patch-storage-class '{"storageClass":"NEARLINE"}'
patch_bucket patch-settings-enable '{"versioning":{"enabled":true},"defaultEventBasedHold":true}'
patch_bucket patch-settings-omitted '{}'
patch_bucket patch-settings-disable '{"versioning":{"enabled":false},"defaultEventBasedHold":false}'
patch_bucket patch-hold-enable '{"defaultEventBasedHold":true}'
patch_bucket patch-hold-disable '{"defaultEventBasedHold":false}'
patch_bucket patch-unknown '{"futureUnknownField":{"enabled":true}}'
for field in cors lifecycle retentionPolicy iamConfiguration; do
  patch_bucket "patch-unsupported-$field" "{\"$field\":null}"
done
patch_bucket patch-malformed '{"labels":'
patch_bucket patch-missing '{}' '' compare-patch-missing
patch_bucket patch-projection-full '{}' '?projection=full'
patch_bucket patch-projection-noacl '{}' '?projection=noAcl'
patch_bucket patch-predefined-acl '{}' '?predefinedAcl=private'
patch_bucket patch-predefined-default-acl '{}' '?predefinedDefaultObjectAcl=private'
patch_bucket patch-user-project '{}' '?userProject=compare-project'
create patch-conditions-create compare-patch-conditions
patch_bucket patch-match-success '{"labels":{"condition":"match"}}' '?ifMetagenerationMatch=1' compare-patch-conditions
patch_bucket patch-match-stale '{"labels":{"condition":"stale"}}' '?ifMetagenerationMatch=1' compare-patch-conditions
patch_bucket patch-notmatch-success '{"labels":{"condition":"notmatch"}}' '?ifMetagenerationNotMatch=1' compare-patch-conditions
patch_bucket patch-notmatch-failure '{"labels":{"condition":"denied"}}' '?ifMetagenerationNotMatch=3' compare-patch-conditions

jq -es '
  [.[] | select(.target | startswith("reference-"))] as $rows
  | [$rows[] | select(.request.method == "PATCH")] as $patches
  | ($patches | length == 58)
    and all($patches[];
      .status == (if .case == "patch-malformed" then 400
        elif .case == "patch-missing" then (if .target == "reference-filesystem" then 500 else 404 end)
        elif .case == "patch-settings-enable" and .target == "reference-filesystem" then 500
        else 200 end))
    and all($patches[] | select(.status == 200) | .body | fromjson;
      (has("labels") | not) and .storageClass == "STANDARD" and .metageneration == "1"
      and .etag == "RVRhZw==" and .updated == .timeCreated)
    and all($rows[] | select(.case == "patch-settings-enable-get-state");
      (.body | fromjson) as $body
      | $body.versioning.enabled == (.target == "reference-memory")
        and $body.defaultEventBasedHold == (.target == "reference-memory"))
    and all($rows[] | select(.case == "patch-settings-omitted-get-state") | .body | fromjson;
      .versioning.enabled == false and .defaultEventBasedHold == false)
    and all($rows[] | select(.case == "patch-hold-enable-get-state") | .body | fromjson;
      .defaultEventBasedHold == true)
    and all($rows[] | select(.case == "patch-hold-disable-get-state") | .body | fromjson;
      .defaultEventBasedHold == false)
' "$work/results.jsonl" >/dev/null
echo 'PATCH-ASSERT reference status, ignored fields, settings and revision behavior: PASS'

if [[ $reference_only == 0 ]]; then
  jq -es '
    def resource($rows; $case): $rows[$case].body | fromjson;
    [group_by(.target)[] | select(.[0].target | startswith("mimic-"))] as $targets
    | ($targets | length == 2) and all($targets[];
      (map({key:.case,value:.}) | from_entries) as $rows
      | [.[] | select(.request.method == "PATCH")] as $patches
      | ($patches | length == 29)
        and all($patches[];
          .status == (if .case == "patch-malformed" then 400
            elif .case == "patch-missing" then 404
            elif (.case | startswith("patch-unsupported-")) or .case == "patch-predefined-acl"
              or .case == "patch-predefined-default-acl" or .case == "patch-user-project" then 400
            elif .case == "patch-match-stale" or .case == "patch-notmatch-failure" then 412
            else 200 end))
        and (resource($rows; "patch-label-add").labels == {keep:"one",remove:"two"})
        and (resource($rows; "patch-label-merge").labels == {keep:"one",remove:"two",add:"three"})
        and (resource($rows; "patch-label-remove").labels == {keep:"one",add:"three"})
        and (resource($rows; "patch-label-empty").labels == {keep:"one",add:"three"})
        and ((resource($rows; "patch-label-null").labels // {}) == {})
        and (resource($rows; "patch-label-empty-value").labels == {keep:"one",empty:""})
        and (resource($rows; "patch-storage-class").storageClass == "NEARLINE")
        and all(["patch-settings-enable","patch-settings-omitted"][];
          resource($rows; .) | .versioning.enabled == true and .defaultEventBasedHold == true)
        and all(["patch-settings-disable","patch-hold-disable"][];
          resource($rows; .) | (.versioning.enabled // false) == false and (.defaultEventBasedHold // false) == false)
        and (resource($rows; "patch-hold-enable").defaultEventBasedHold == true)
        and (resource($rows; "patch-notmatch-failure-get-state").labels == {condition:"notmatch"})
        and (reduce $patches[] as $patch (
          {ok:true, previous:{
            "compare-patch":resource($rows; "patch-create"),
            "compare-patch-conditions":resource($rows; "patch-conditions-create")}};
          ($patch.request.path | split("?")[0] | split("/")[-1]) as $name
          | resource($rows; $patch.case + "-get-state") as $after
          | .previous[$name] as $before
          | if $name == "compare-patch-missing" then
              .ok = (.ok and $rows[$patch.case + "-get-state"].status == 404)
            else
              .ok = (.ok and
                (resource($rows; $patch.case + "-list-state").items | map(select(.name == $name))) == [$after]
                and (if $patch.status == 200 then
                  ($patch.request.body | fromjson) as $input
                  | ($patch.body | fromjson) == $after
                  and ($after.metageneration | tonumber) == (($before.metageneration | tonumber) + 1)
                  and $after.updated >= $before.updated and $after.updated >= $after.timeCreated and $after.etag != $before.etag
                  and $after.timeCreated == $before.timeCreated and $after.name == $before.name
                  and $after.location == $before.location and $after.projectNumber == $before.projectNumber
                  and $after.storageClass == ($input.storageClass // $before.storageClass)
                  and (if $input | has("labels") then true else ($after.labels // {}) == ($before.labels // {}) end)
                  and (($after.versioning.enabled // false) ==
                    (if $input | has("versioning") then $input.versioning.enabled else ($before.versioning.enabled // false) end))
                  and (($after.defaultEventBasedHold // false) ==
                    (if $input | has("defaultEventBasedHold") then $input.defaultEventBasedHold else ($before.defaultEventBasedHold // false) end))
                else $after == $before end))
              | .previous[$name] = $after
            end) | .ok))
  ' "$work/results.jsonl" >/dev/null
  echo 'PATCH-ASSERT mimic profiles status, merge/removal, settings, revision and failure preservation: PASS'
fi

put_bucket() {
  local label=$1 body=$2 query=${3:-} name=${4:-compare-put}
  request "$label" PUT "/storage/v1/b/$name$query" "$body"
  state "$label" "$name"
}

create put-create compare-put '{"defaultEventBasedHold":true}'
request put-upload POST '/upload/storage/v1/b/compare-put/o?uploadType=media&name=kept.txt' 'PUT preserved payload' text/plain
state put-before compare-put
request put-object-before GET /storage/v1/b/compare-put/o/kept.txt
put_bucket put-all-fields '{"labels":{"keep":"one","remove":"two"},"storageClass":"NEARLINE","versioning":{"enabled":true},"defaultEventBasedHold":true}'
put_bucket put-label-replace '{"labels":{"only":"replacement"}}'
put_bucket put-label-empty '{"labels":{}}'
put_bucket put-label-null '{"labels":null}'
put_bucket put-settings-enable '{"versioning":{"enabled":true},"defaultEventBasedHold":true}'
put_bucket put-settings-omitted '{}'
put_bucket put-settings-disable '{"versioning":{"enabled":false},"defaultEventBasedHold":false}'
request put-object-after-disable GET /storage/v1/b/compare-put/o/kept.txt
put_bucket put-versioning-empty '{"versioning":{}}'
put_bucket put-settings-null '{"versioning":null,"defaultEventBasedHold":null}'
put_bucket put-class-null '{"storageClass":null}'
put_bucket put-unknown '{"futureUnknownField":{"enabled":true}}'
for field in cors lifecycle retentionPolicy iamConfiguration; do
  put_bucket "put-unsupported-$field" "{\"$field\":null}"
done
put_bucket put-empty-acl '{"acl":[]}'
put_bucket put-nonempty-acl '{"acl":[{"entity":"allUsers","role":"READER"}]}'
put_bucket put-default-acl '{"defaultObjectAcl":[{"entity":"allUsers","role":"READER"}]}'
put_bucket put-empty-lifecycle '{"lifecycle":{"rule":[]}}'
put_bucket put-malformed '{"labels":'
put_bucket put-missing '{}' '' compare-put-missing
put_bucket put-projection-full '{}' '?projection=full'
put_bucket put-projection-noacl '{}' '?projection=noAcl'
put_bucket put-projection-invalid '{}' '?projection=invalid'
put_bucket put-predefined-acl '{}' '?predefinedAcl=private'
put_bucket put-predefined-default-acl '{}' '?predefinedDefaultObjectAcl=private'
put_bucket put-user-project '{}' '?userProject=compare-project'
put_bucket put-generation '{}' '?generation=1'
create put-conditions-create compare-put-conditions
put_bucket put-match-success '{"labels":{"condition":"match"}}' '?ifMetagenerationMatch=1' compare-put-conditions
put_bucket put-match-stale '{"labels":{"condition":"stale"}}' '?ifMetagenerationMatch=1' compare-put-conditions
put_bucket put-notmatch-success '{"labels":{"condition":"notmatch"}}' '?ifMetagenerationNotMatch=1' compare-put-conditions
put_bucket put-notmatch-failure '{"labels":{"condition":"denied"}}' '?ifMetagenerationNotMatch=3' compare-put-conditions
request put-objects GET /storage/v1/b/compare-put/o
request put-object-bytes GET '/storage/v1/b/compare-put/o/kept.txt?alt=media'

request fields-bucket GET '/storage/v1/b/compare-put?fields=name'
request fields-object GET '/storage/v1/b/compare-put/o/kept.txt?fields=name,size'
request fields-list GET '/storage/v1/b/compare-put/o?fields=items(name),nextPageToken'
request etag-none-bucket GET /storage/v1/b/compare-put '' application/json 'If-None-Match: *'
request etag-none-object GET /storage/v1/b/compare-put/o/kept.txt '' application/json 'If-None-Match: *'
request etag-none-media GET '/storage/v1/b/compare-put/o/kept.txt?alt=media' '' application/json 'If-None-Match: *'
request etag-match-bucket GET /storage/v1/b/compare-put '' application/json 'If-Match: "stale"'
request etag-match-object GET /storage/v1/b/compare-put/o/kept.txt '' application/json 'If-Match: "stale"'

jq -es '
  group_by(.target) | all(.[];
    (map({key:.case,value:.}) | from_entries) as $rows
    | if .[0].target | startswith("reference-") then
        ([.[] | select(.case | startswith("fields-") or startswith("etag-"))] | length == 8)
        and all(.[] | select(.case | startswith("fields-") or startswith("etag-")); .status == 200)
        and ($rows["fields-bucket"].body | fromjson | keys | length > 1)
        and ($rows["fields-object"].body | fromjson | keys | length > 2)
      else
        ($rows["fields-bucket"].body | fromjson) == {name:"compare-put"}
        and ($rows["fields-object"].body | fromjson | keys) == ["name","size"]
        and ($rows["fields-list"].body | fromjson) == {items:[{name:"kept.txt"}]}
        and all(.[] | select(.case | startswith("etag-none-")); .status == 304 and .body == "")
        and all(.[] | select(.case | startswith("etag-match-"));
          .status == 412 and (.body | fromjson | .error.errors[0].reason) == "conditionNotMet")
      end)
' "$work/results.jsonl" >/dev/null
echo 'FIELDS-ETAG-ASSERT reference backends ignore selectors/validators; mimic profiles select fields and enforce 304/412: PASS'

# The pinned server.go registers bucket PATCH and its POST override, but no bucket PUT.
# bucket.go updateBucket therefore cannot establish PUT replacement semantics.
jq -es '
  [group_by(.target)[] | select(.[0].target | startswith("reference-"))] as $targets
  | ($targets | length == 2) and all($targets[];
    (map({key:.case,value:.}) | from_entries) as $rows
    | [.[] | select(.request.method == "PUT")] as $puts
    | ($puts | length == 32) and all($puts[];
        .status == 404 and (.body | fromjson) == {error:{code:404,message:"Not Found",
          errors:[{domain:"global",reason:"notFound",message:"Not Found"}]}})
      and all($puts[] | select(.case != "put-missing");
        (.request.path | split("?")[0] | split("/")[-1]) as $name
        | ($rows[.case + "-get-state"].body | fromjson) as $after
        | ($rows[if $name == "compare-put" then "put-before-get-state" else "put-conditions-create-get-state" end].body | fromjson) as $before
        | $after == $before
          and ($rows[.case + "-list-state"].body | fromjson | .items | map(select(.name == $name))) == [$after])
      and $rows["put-missing-get-state"].status == 404
      and ($rows["put-objects"].body | fromjson | .items | map(.name)) == ["kept.txt"]
      and $rows["put-object-bytes"].body == "PUT preserved payload")
' "$work/results.jsonl" >/dev/null
echo 'PUT-ASSERT reference backends reject bucket PUT with JSON 404 and preserve metadata/objects: PASS'

if [[ $reference_only == 0 ]]; then
  jq -es '
    def resource($rows; $case): $rows[$case].body | fromjson;
    [group_by(.target)[] | select(.[0].target | startswith("mimic-"))] as $targets
    | ($targets | length == 2) and all($targets[];
      (map({key:.case,value:.}) | from_entries) as $rows
      | [.[] | select(.request.method == "PUT")] as $puts
      | ($puts | length == 32) and all($puts[];
          (if .case == "put-malformed" then [400,(if .target == "mimic-native" then "parseError" else "invalid" end)]
            elif .case == "put-class-null" then [400,(if .target == "mimic-native" then "invalidArgument" else "invalid" end)]
            elif .case == "put-settings-null" and .target == "mimic-native" then [400,"invalidArgument"]
            elif .case == "put-projection-invalid" or .case == "put-generation" then [400,"invalidParameter"]
            elif .case == "put-missing" then [404,"notFound"]
            elif (.case | startswith("put-unsupported-")) or .case == "put-nonempty-acl" or .case == "put-default-acl"
              or .case == "put-predefined-acl" or .case == "put-predefined-default-acl" or .case == "put-user-project" then [400,"badRequest"]
            elif .case == "put-match-stale" or .case == "put-notmatch-failure" then [412,"conditionNotMet"]
            else [200,null] end) as $expected
          | .status == $expected[0]
            and (if .status == 200 then true
              else (.body | fromjson | .error.errors[0].reason) == $expected[1] end))
        and resource($rows; "put-create-get-state") == resource($rows; "put-before-get-state")
        and resource($rows; "put-object-before").eventBasedHold == true
        and resource($rows; "put-object-before") == resource($rows; "put-object-after-disable")
        and (resource($rows; "put-all-fields").labels == {keep:"one",remove:"two"})
        and (resource($rows; "put-label-replace").labels == {only:"replacement"})
        and (resource($rows; "put-notmatch-failure-get-state").labels == {condition:"notmatch"})
        and (reduce $puts[] as $put (
          {ok:true, previous:{"compare-put":resource($rows; "put-before-get-state"),
            "compare-put-conditions":resource($rows; "put-conditions-create-get-state")}};
          ($put.request.path | split("?")[0] | split("/")[-1]) as $name
          | resource($rows; $put.case + "-get-state") as $after
          | .previous[$name] as $before
          | if $name == "compare-put-missing" then
              .ok = (.ok and $rows[$put.case + "-get-state"].status == 404)
            else
              .ok = (.ok and
                (resource($rows; $put.case + "-list-state").items | map(select(.name == $name))) == [$after]
                and (if $put.status == 200 then
                  ($put.request.body | fromjson) as $input
                  | ($put.body | fromjson) == $after
                    and ($after.metageneration | tonumber) == (($before.metageneration | tonumber) + 1)
                    and $after.etag != $before.etag and $after.updated >= $before.updated
                    and $after.updated >= $after.timeCreated and $after.timeCreated == $before.timeCreated
                    and $after.name == $before.name and $after.id == $before.id
                    and $after.location == $before.location and $after.projectNumber == $before.projectNumber
                    and ($after.labels // {}) == (if $input | has("labels") then $input.labels // {} else $before.labels // {} end)
                    and $after.storageClass == ($input.storageClass // $before.storageClass)
                    and ($after.versioning.enabled // false) == (if $input | has("versioning") then $input.versioning.enabled // false else $before.versioning.enabled // false end)
                    and ($after.defaultEventBasedHold // false) == (if $input | has("defaultEventBasedHold") then $input.defaultEventBasedHold // false else $before.defaultEventBasedHold // false end)
                else $after == $before end))
              | .previous[$name] = $after
            end) | .ok)
        and (resource($rows; "put-objects").items | map(.name)) == ["kept.txt"]
        and $rows["put-object-bytes"].body == "PUT preserved payload")
  ' "$work/results.jsonl" >/dev/null
  echo 'PUT-ASSERT mimic profiles preserve omitted metadata, enforce conditions and preserve objects: PASS'
fi

echo
echo 'Comparison table (HTTP statuses; equality includes selected headers and normalized bodies):'
jq -rs --argjson targets "$(printf '%s\n' "${targets[@]}" | jq -Rsc 'split("\n")[:-1]')" '
  . as $rows | (["case"] + $targets + ["reference equal", "all equal"]) as $head
  | ($head | join(" | ")), ($head | map("---") | join(" | ")),
    ($rows | group_by(.case)[] | . as $group
      | [$group[0].case] + [$targets[] as $target | $group[] | select(.target == $target) | .status | tostring]
        + [([$group[] | select(.target | startswith("reference-")) | .comparison]
             | unique | length == 1 | tostring),
           ([$group[].comparison] | unique | length == 1 | tostring)] | join(" | "))
' "$work/results.jsonl"
for target in "${targets[@]}"; do
  jq -rsc --arg target "$target" '
    [.[] | select(.target == $target)] as $rows
    | ($rows[] | select(.case == "name-only") | .comparison.body) as $created
    | ($rows[] | select(.case == "get-existing") | .comparison.body) as $fetched
    | "RESOURCE-COMPARE \($target) create/get normalized bodies equal: \($created == $fetched)"
  ' "$work/results.jsonl"
done

reference_host_cases
