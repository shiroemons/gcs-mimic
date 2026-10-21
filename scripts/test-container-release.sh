#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
script="$PWD/scripts/container-release.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin" "$work/digests"
export GHCR_TEST_ROOT="$work"
export PATH="$work/bin:$PATH"
export IMAGE=ghcr.io/example/gcs-mimic GH_TOKEN=mock-secret-do-not-print
export GITHUB_REPOSITORY=example/gcs-mimic GITHUB_REF_NAME=v0.2.3 GITHUB_ACTOR=example
printf -v zeros '%064d' 0
export GITHUB_SHA=${zeros:0:40}
export GHCR_TEST_AMD64="sha256:${zeros//0/a}" GHCR_TEST_ARM64="sha256:${zeros//0/b}"
export GHCR_TEST_INDEX="sha256:${zeros//0/9}"
export GITHUB_STEP_SUMMARY="$work/summary"
jq -n --arg digest "sha256:${zeros//0/c}" --arg attestation "sha256:${zeros//0/1}" '
  {schemaVersion:2,mediaType:"application/vnd.oci.image.index.v1+json",manifests:[
    {digest:$digest,size:100,mediaType:"application/vnd.oci.image.manifest.v1+json",platform:{os:"linux",architecture:"amd64"}},
    {digest:$attestation,size:100,mediaType:"application/vnd.oci.image.manifest.v1+json",platform:{os:"unknown",architecture:"unknown"},
     annotations:{"vnd.docker.reference.type":"attestation-manifest","vnd.docker.reference.digest":$digest}}]}
' > "$work/amd64.json"
jq --arg digest "sha256:${zeros//0/d}" --arg attestation "sha256:${zeros//0/2}" '
  .manifests[0].platform.architecture="arm64" | .manifests[0].digest=$digest
  | .manifests[1].digest=$attestation | .manifests[1].annotations["vnd.docker.reference.digest"]=$digest
' "$work/amd64.json" > "$work/arm64.json"
jq -s '.[0].manifests += .[1].manifests | .[0]' "$work/amd64.json" "$work/arm64.json" > "$work/merged.json"

cat > "$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2 $3" == 'xtask release container-tags' ]] || exit 1
[[ "$4" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || exit 1
printf '%s\n' "$@" > "$GHCR_TEST_ROOT/policy-arguments"
if [[ $# == 4 ]]; then
  printf '%s\n' "$4" "${4%.*}" latest
else
  printf '%s\n' "$GHCR_TEST_TAGS"
fi
SH
cat > "$work/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GHCR_TEST_ROOT/gh-reads"
if [[ -n "$GHCR_TEST_API_FAIL" && "$*" == *"$GHCR_TEST_API_FAIL"* ]]; then
  echo 'Mock GitHub API failure' >&2
  exit 23
fi
case "$1 $2" in
  'api repos/example/gcs-mimic') printf '%s\n' "$GHCR_TEST_OWNER" ;;
  'api users/example/packages/container/gcs-mimic'|'api orgs/example/packages/container/gcs-mimic')
    printf '%s\n' "$GHCR_TEST_VISIBILITY" ;;
  'api --paginate')
    if [[ "$3" == */versions\?per_page=100 ]]; then
      printf '%s\n' "$GHCR_TEST_EXISTING"
    elif [[ "$3" == repos/example/gcs-mimic/releases\?per_page=100 ]]; then
      printf '%s\n' "$GHCR_TEST_RELEASES"
    else exit 90; fi ;;
  'api repos/example/gcs-mimic/git/ref/tags/v0.2.3')
    jq -n --arg type "$GHCR_TEST_REF_TYPE" --arg sha "$GHCR_TEST_REF_SHA" '{object:{type:$type,sha:$sha}}' ;;
  api\ repos/example/gcs-mimic/git/tags/*)
    jq -n --arg sha "$GITHUB_SHA" '{object:{type:"commit",sha:$sha}}' ;;
  'release view') if [[ "$GHCR_TEST_ASSET" != absent ]]; then echo CONTAINER-DIGESTS.json; fi ;;
  'release download')
    while [[ "$1" != --dir ]]; do shift; done
    if [[ "$GHCR_TEST_ASSET" == different ]]; then echo '{}' > "$2/CONTAINER-DIGESTS.json"
    else cp "$GHCR_TEST_ROOT/uploaded.json" "$2/CONTAINER-DIGESTS.json"; fi ;;
  'release upload')
    [[ "$*" != *--clobber* ]] || exit 1
    printf 'gh:upload\n' >> "$GHCR_TEST_ROOT/mutations"
    cp "$4" "$GHCR_TEST_ROOT/uploaded.json" ;;
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
  printf 'docker:create %s\n' "$*" >> "$GHCR_TEST_ROOT/mutations"
  exit 0
fi
[[ "$action" == inspect ]] || exit 1
format=$1
if [[ "$format" == --raw ]]; then
  case "$2" in
    *"@$GHCR_TEST_AMD64") cat "$GHCR_TEST_ROOT/amd64-current.json" ;;
    *"@$GHCR_TEST_ARM64") cat "$GHCR_TEST_ROOT/arm64.json" ;;
    *:0.2.3) cat "$GHCR_TEST_ROOT/version.json" ;;
    *) exit 92 ;;
  esac
elif [[ "$format" == --format ]]; then
  case "$2" in
    '{{.Manifest.Digest}}')
      if [[ "${DOCKER_CONFIG:-}" == */anonymous && "$GHCR_TEST_ANONYMOUS" == fail ]]; then exit 24; fi
      echo "$GHCR_TEST_INDEX" ;;
    '{{json .SBOM}}')
      if [[ "$GHCR_TEST_SBOM" == missing ]]; then echo null
      else echo '{"SPDX":{"spdxVersion":"SPDX-2.3"}}'; fi ;;
    '{{json .Provenance}}')
      if [[ "$GHCR_TEST_PROVENANCE" == missing ]]; then echo '{}'
      elif [[ "$GHCR_TEST_PROVENANCE" == v0.2 ]]; then echo '{"SLSA":{"buildType":"https://mobyproject.org/buildkit@v1"}}'
      else echo '{"SLSA":{"buildDefinition":{"buildType":"https://mobyproject.org/buildkit@v1"}}}'; fi ;;
    *) exit 93 ;;
  esac
else exit 94; fi
SH
chmod +x "$work/bin/cargo" "$work/bin/gh" "$work/bin/docker"

reset_case() {
  : > "$work/mutations"
  : > "$work/gh-reads"
  export GHCR_TEST_OWNER=User GHCR_TEST_VISIBILITY=public GHCR_TEST_API_FAIL=''
  export GHCR_TEST_EXISTING='' GHCR_TEST_RELEASES=0.2.3 GHCR_TEST_ASSET=absent
  export GHCR_TEST_TAGS=$'0.2.3\n0.2\nlatest'
  export GHCR_TEST_REF_TYPE=commit GHCR_TEST_REF_SHA="$GITHUB_SHA"
  export GHCR_TEST_SBOM=present GHCR_TEST_PROVENANCE=present GHCR_TEST_ANONYMOUS=pass
  cp "$work/amd64.json" "$work/amd64-current.json"
  cp "$work/merged.json" "$work/version.json"
  printf '%s\n' "$GHCR_TEST_AMD64" > "$work/digests/amd64.digest"
  printf '%s\n' "$GHCR_TEST_ARM64" > "$work/digests/arm64.digest"
  rm -f "$work/digests/extra"
}
run_case() {
  local result=0
  bash "$script" 0.2.3 "$work/digests" > "$work/output" 2>&1 || result=$?
  if grep -F "$GH_TOKEN" "$work/output" "$work/gh-reads" "$work/mutations"; then
    echo 'A registry token was exposed' >&2
    exit 1
  fi
  return "$result"
}
fail_before_mutation() {
  if run_case; then echo 'Invalid publication unexpectedly succeeded' >&2; exit 1; fi
  test ! -s "$work/mutations"
}

reset_case
printf 'invalid\n' > "$work/digests/amd64.digest"
fail_before_mutation
reset_case
: > "$work/digests/extra"
fail_before_mutation
reset_case
export GHCR_TEST_VISIBILITY=private
fail_before_mutation
grep -F 'Set this package to Public' "$work/output" > /dev/null
for failure in '/versions?' '/releases?' 'release view'; do
  reset_case
  export GHCR_TEST_API_FAIL="$failure"
  fail_before_mutation
done
reset_case
export GHCR_TEST_REF_SHA=${zeros:0:39}1
fail_before_mutation
reset_case
jq '.manifests[0].platform.architecture="arm64"' "$work/amd64.json" > "$work/amd64-current.json"
fail_before_mutation
reset_case
export GHCR_TEST_SBOM=missing
fail_before_mutation
reset_case
export GHCR_TEST_PROVENANCE=missing
fail_before_mutation
reset_case
export GHCR_TEST_PROVENANCE=v0.2
run_case
test "$(wc -l < "$work/mutations")" -eq 4
reset_case
export GHCR_TEST_EXISTING=0.2.3
jq '.manifests |= reverse | .manifests[0].size=999' "$work/merged.json" > "$work/version.json"
fail_before_mutation

reset_case
export GHCR_TEST_REF_TYPE=tag
run_case
grep -F "docker:create --tag $IMAGE:0.2.3 $IMAGE@$GHCR_TEST_AMD64 $IMAGE@$GHCR_TEST_ARM64" "$work/mutations" > /dev/null
grep -F "docker:create --tag $IMAGE:0.2 $IMAGE@$GHCR_TEST_INDEX" "$work/mutations" > /dev/null
grep -F "docker:create --tag $IMAGE:latest $IMAGE@$GHCR_TEST_INDEX" "$work/mutations" > /dev/null
test "$(wc -l < "$work/mutations")" -eq 4
jq -e --arg digest "$GHCR_TEST_INDEX" --arg revision "$GITHUB_SHA" \
  '.index_digest == $digest and .source_commit == $revision and (.platform_manifests | length == 2) and .attestations.signed == false' "$work/uploaded.json" > /dev/null

reset_case
export GHCR_TEST_OWNER=Organization GHCR_TEST_EXISTING=0.2.10 GHCR_TEST_TAGS=0.2.3
run_case
grep -F 'api --paginate orgs/example/packages/container/gcs-mimic/versions' "$work/gh-reads" > /dev/null
test "$(wc -l < "$work/mutations")" -eq 2
if grep -E 'gcs-mimic:(0\.2 |latest )' "$work/mutations"; then exit 1; fi
grep -Fx 0.2.10 "$work/policy-arguments" > /dev/null

reset_case
export GHCR_TEST_RELEASES=0.3.0 GHCR_TEST_TAGS=$'0.2.3\n0.2'
run_case
test "$(wc -l < "$work/mutations")" -eq 3
if grep -F 'gcs-mimic:latest ' "$work/mutations"; then exit 1; fi
grep -Fx 0.3.0 "$work/policy-arguments" > /dev/null

reset_case
export GHCR_TEST_EXISTING=0.2.3 GHCR_TEST_ASSET=same
run_case
test "$(wc -l < "$work/mutations")" -eq 2
if grep -E 'gh:upload|gcs-mimic:0\.2\.3 ' "$work/mutations"; then exit 1; fi
reset_case
export GHCR_TEST_EXISTING=0.2.3 GHCR_TEST_ASSET=different
fail_before_mutation

reset_case
export GHCR_TEST_ANONYMOUS=fail
if run_case; then echo 'Anonymous-access failure unexpectedly succeeded' >&2; exit 1; fi
test "$(wc -l < "$work/mutations")" -eq 1
reset_case
export GHCR_TEST_EXISTING=0.2.3
run_case
test "$(wc -l < "$work/mutations")" -eq 3
echo 'PASS: container publication validates inputs, API failures, immutable retries, attestations and alias selection without network access'
