#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
repository=${GCS_MIMIC_REFERENCE_REPO:?Set GCS_MIMIC_REFERENCE_REPO to the pinned repository URL listed in the README comparison sources}
revision=896ece51e49eed0946089f3089f6343855c0d106
archive_sha256=13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc
go_toolchain=$(awk '$1 == "toolchain" {print $2}' sdk-tests/go/go.mod)
reference_only=${GCS_MIMIC_REFERENCE_ONLY:-0}
[[ $reference_only == 0 || $reference_only == 1 ]]
for command in curl jq go cargo tar shasum git; do
  if ! command -v "$command" >/dev/null; then echo "Missing command: $command" >&2; exit 1; fi
done
[[ $go_toolchain =~ ^go[0-9]+\.[0-9]+\.[0-9]+$ ]]
work=$(mktemp -d "${TMPDIR:?Set TMPDIR to a temporary directory}/gcs-mimic-compare.XXXXXX")
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
      -filesystem-root "$work/$target-data" -data "$work/no-seed" \
      >"$work/$target.log" 2>&1 &
  else
    env -i PATH="$PATH" "$server" serve --listen "$address" --api-base-url "$origin" \
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

printf 'pinned reference emulator v1.56.1 commit %s\narchive SHA256 %s\n' "$revision" "$archive_sha256"
go version
printf 'checkout source commit %s (reference-only=%s)\n' "$(git rev-parse HEAD)" "$reference_only"
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
  local label=$1 method=$2 path=$3 body=${4-} content_type=${5:-application/json}
  local index target origin status result
  local args=()
  if [[ $method == POST || $method == PATCH ]]; then args=(--data-binary "$body" --header "Content-Type: $content_type"); fi
  for index in "${!targets[@]}"; do
    target=${targets[$index]}
    origin=${origins[$index]}
    status=$(curl --noproxy '*' --proxy '' --globoff --path-as-is --silent --show-error \
      --connect-timeout 2 --max-time 15 --request "$method" ${args[@]+"${args[@]}"} \
      --dump-header "$work/headers" --output "$work/body" --write-out '%{http_code}' "$origin$path")
    [[ $status =~ ^[1-5][0-9][0-9]$ ]]
    result=$(jq -nSc --arg target "$target" --arg label "$label" --arg method "$method" \
      --arg path "$path" --arg request_body "$body" --arg origin "$origin" --argjson status "$status" \
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
      | {target:$target, case:$label, request:{method:$method,path:$path,body:$request_body},
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
patch_bucket patch-storage-class '{"storageClass":"NEARLINE"}'
patch_bucket patch-settings-enable '{"versioning":{"enabled":true},"defaultEventBasedHold":true}'
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
  | ($patches | length == 54)
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
    and all($rows[] | select(.case == "patch-hold-enable-get-state") | .body | fromjson;
      .defaultEventBasedHold == true)
    and all($rows[] | select(.case == "patch-hold-disable-get-state") | .body | fromjson;
      .defaultEventBasedHold == false)
' "$work/results.jsonl" >/dev/null
echo 'PATCH-ASSERT reference status, ignored fields, settings and revision behavior: PASS'

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
