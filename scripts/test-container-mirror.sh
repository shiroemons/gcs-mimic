#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
script="$PWD/scripts/container-mirror.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
export MIRROR_TEST_ROOT="$work"
export PATH="$work/bin:$PATH"
export SOURCE_IMAGE=ghcr.io/example/gcs-mimic IMAGE=docker.io/example/gcs-mimic
export GH_TOKEN=mock-secret-do-not-print GITHUB_REPOSITORY=example/gcs-mimic
printf -v zeros '%064d' 0
export MIRROR_TEST_COMMIT=${zeros:0:40}
export MIRROR_TEST_INDEX="sha256:${zeros//0/9}"
export MIRROR_TEST_HUB=https://hub.docker.com/v2/namespaces/example/repositories/gcs-mimic
export GITHUB_STEP_SUMMARY="$work/summary"

cat > "$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2 $3" == 'xtask release container-tags' ]] || exit 1
printf '%s\n' "$@" > "$MIRROR_TEST_ROOT/policy-arguments"
printf '%s\n' "$MIRROR_TEST_TAGS"
SH
cat > "$work/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
url=${!#}
printf '%s\n' "$url" >> "$MIRROR_TEST_ROOT/curl-reads"
if [[ -n "$MIRROR_TEST_API_FAIL" && "$url" == *"$MIRROR_TEST_API_FAIL"* ]]; then
  echo 'Mock Docker Hub API failure' >&2
  exit 22
fi
case "$url" in
  "$MIRROR_TEST_HUB") jq -n --argjson private "$MIRROR_TEST_PRIVATE" '{is_private:$private}' ;;
  "$MIRROR_TEST_HUB/tags?page_size=100")
    jq -n --arg next "$MIRROR_TEST_NEXT" --arg tags "$MIRROR_TEST_EXISTING" '
      {next:(if $next == "" then null else $next end),
       results:[$tags | split(" ")[] | select(. != "") | {name:.}]}' ;;
  "$MIRROR_TEST_HUB/tags?page=2&page_size=100")
    jq -n --arg tags "$MIRROR_TEST_EXISTING_PAGE2" '{next:null,results:[$tags | split(" ")[] | {name:.}]}' ;;
  *) echo 'Unexpected mock curl URL' >&2; exit 90 ;;
esac
SH
cat > "$work/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$MIRROR_TEST_ROOT/gh-reads"
case "$1 $2" in
  'release download')
    while [[ "$1" != --dir ]]; do shift; done
    jq -n --arg image "$MIRROR_TEST_ASSET_IMAGE" --arg digest "$MIRROR_TEST_INDEX" --arg commit "$MIRROR_TEST_COMMIT" '
      {schema_version:1,image:$image,version:"0.2.3",source_commit:$commit,index_digest:$digest}
    ' > "$2/CONTAINER-DIGESTS.json" ;;
  'api repos/example/gcs-mimic/git/ref/tags/v0.2.3')
    jq -n --arg sha "$MIRROR_TEST_REF_SHA" '{object:{type:"commit",sha:$sha}}' ;;
  'api --paginate') printf '%s\n' "$MIRROR_TEST_RELEASES" ;;
  *) echo 'Unexpected mock gh command' >&2; exit 91 ;;
esac
SH
cat > "$work/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2" == 'buildx imagetools' ]] || exit 1
action=$3
shift 3
if [[ "$action" == create ]]; then
  printf 'docker:create %s\n' "$*" >> "$MIRROR_TEST_ROOT/mutations"
  exit 0
fi
[[ "$action" == inspect && "$1" == --format ]] || exit 1
case "$2 $3" in
  "{{.Manifest.Digest}} $SOURCE_IMAGE:0.2.3") echo "$MIRROR_TEST_SOURCE_DIGEST" ;;
  "{{.Manifest.Digest}} $IMAGE:0.2.3") echo "$MIRROR_TEST_HUB_DIGEST" ;;
  '{{.Manifest.Digest}} '*)
    if [[ "${DOCKER_CONFIG:-}" == */anonymous && "$MIRROR_TEST_ANONYMOUS" == fail ]]; then exit 24; fi
    echo "$MIRROR_TEST_INDEX" ;;
  "{{json .SBOM}} $SOURCE_IMAGE@"*)
    if [[ "$MIRROR_TEST_SBOM" == missing ]]; then echo '{"linux/amd64":{"SPDX":{"spdxVersion":"SPDX-2.3"}}}'
    else echo '{"linux/amd64":{"SPDX":{"spdxVersion":"SPDX-2.3"}},"linux/arm64":{"SPDX":{"spdxVersion":"SPDX-2.3"}}}'; fi ;;
  "{{json .SBOM}} $IMAGE@"*)
    echo '{"linux/amd64":{"SPDX":{"spdxVersion":"SPDX-2.3"}},"linux/arm64":{"SPDX":{"spdxVersion":"SPDX-2.3"}}}' ;;
  '{{json .Provenance}} '*)
    if [[ "$MIRROR_TEST_PROVENANCE" == foreign ]]; then build=https://example.invalid/builder
    else build=https://github.com/moby/buildkit/blob/master/docs/attestations/slsa-definitions.md; fi
    jq -n --arg build "$build" '{"linux/amd64":{SLSA:{buildDefinition:{buildType:$build}}},"linux/arm64":{SLSA:{buildType:"https://mobyproject.org/buildkit@v1"}}}' ;;
  *) exit 92 ;;
esac
SH
chmod +x "$work/bin/cargo" "$work/bin/curl" "$work/bin/gh" "$work/bin/docker"

reset_case() {
  : > "$work/mutations"
  : > "$work/gh-reads"
  : > "$work/curl-reads"
  export MIRROR_TEST_PRIVATE=false MIRROR_TEST_API_FAIL='' MIRROR_TEST_NEXT=''
  export MIRROR_TEST_EXISTING='' MIRROR_TEST_EXISTING_PAGE2='' MIRROR_TEST_RELEASES=0.2.3
  export MIRROR_TEST_TAGS=$'0.2.3\n0.2\nlatest' MIRROR_TEST_ASSET_IMAGE="$SOURCE_IMAGE"
  export MIRROR_TEST_REF_SHA="$MIRROR_TEST_COMMIT" MIRROR_TEST_SOURCE_DIGEST="$MIRROR_TEST_INDEX"
  export MIRROR_TEST_HUB_DIGEST="$MIRROR_TEST_INDEX" MIRROR_TEST_SBOM=present
  export MIRROR_TEST_PROVENANCE=present MIRROR_TEST_ANONYMOUS=pass
}
run_case() {
  local result=0
  bash "$script" "${1:-0.2.3}" > "$work/output" 2>&1 || result=$?
  if grep -F "$GH_TOKEN" "$work/output" "$work/gh-reads" "$work/curl-reads" "$work/mutations"; then
    echo 'A registry token was exposed' >&2
    exit 1
  fi
  return "$result"
}
fail_before_mutation() {
  if run_case "$@"; then echo 'Invalid mirror unexpectedly succeeded' >&2; exit 1; fi
  test ! -s "$work/mutations"
}

reset_case
fail_before_mutation 0.2.3-rc.1
reset_case
export MIRROR_TEST_PRIVATE=true
fail_before_mutation
grep -F 'Create a public Docker Hub repository' "$work/output" > /dev/null
for failure in "$MIRROR_TEST_HUB" 'page_size=100'; do
  reset_case
  export MIRROR_TEST_API_FAIL="$failure"
  fail_before_mutation
done
reset_case
export MIRROR_TEST_ASSET_IMAGE=ghcr.io/other/gcs-mimic
fail_before_mutation
reset_case
export MIRROR_TEST_REF_SHA=${zeros:0:39}1
fail_before_mutation
reset_case
export MIRROR_TEST_SOURCE_DIGEST="sha256:${zeros//0/8}"
fail_before_mutation
grep -F 'GHCR version tag does not match' "$work/output" > /dev/null
reset_case
export MIRROR_TEST_SBOM=missing
fail_before_mutation
reset_case
export MIRROR_TEST_PROVENANCE=foreign
fail_before_mutation
reset_case
export MIRROR_TEST_NEXT=https://example.invalid/tags
fail_before_mutation
reset_case
export MIRROR_TEST_EXISTING=0.2.3 MIRROR_TEST_HUB_DIGEST="sha256:${zeros//0/8}"
fail_before_mutation
grep -F 'immutable version tag on Docker Hub' "$work/output" > /dev/null

reset_case
run_case
grep -Fx "docker:create --tag $IMAGE:0.2.3 $SOURCE_IMAGE@$MIRROR_TEST_INDEX" "$work/mutations" > /dev/null
grep -Fx "docker:create --tag $IMAGE:0.2 $IMAGE@$MIRROR_TEST_INDEX" "$work/mutations" > /dev/null
grep -Fx "docker:create --tag $IMAGE:latest $IMAGE@$MIRROR_TEST_INDEX" "$work/mutations" > /dev/null
test "$(wc -l < "$work/mutations")" -eq 3
grep -F "$MIRROR_TEST_INDEX" "$work/summary" > /dev/null

reset_case
export MIRROR_TEST_NEXT="$MIRROR_TEST_HUB/tags?page=2&page_size=100" MIRROR_TEST_EXISTING=0.2
export MIRROR_TEST_EXISTING_PAGE2='0.2.3 latest'
run_case
test "$(wc -l < "$work/mutations")" -eq 2
if grep -F "gcs-mimic:0.2.3 " "$work/mutations"; then exit 1; fi
grep -Fx 0.2.3 "$work/policy-arguments" > /dev/null

reset_case
export MIRROR_TEST_RELEASES=$'0.2.3\n0.3.0' MIRROR_TEST_TAGS=$'0.2.3\n0.2'
run_case
test "$(wc -l < "$work/mutations")" -eq 2
if grep -F 'gcs-mimic:latest ' "$work/mutations"; then exit 1; fi
grep -Fx 0.3.0 "$work/policy-arguments" > /dev/null

reset_case
export MIRROR_TEST_TAGS=$'0.2.3\nedge'
fail_before_mutation

reset_case
export MIRROR_TEST_ANONYMOUS=fail
if run_case; then echo 'Anonymous-access failure unexpectedly succeeded' >&2; exit 1; fi
test "$(wc -l < "$work/mutations")" -eq 1
echo 'PASS: Docker Hub mirroring validates the digest report, source attestations, immutable tags, API failures and alias selection without network access'
