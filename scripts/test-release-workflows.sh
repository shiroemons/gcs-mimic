#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
export WORKFLOW_TEST_ROOT="$work"
export PATH="$work/bin:$PATH"
export REF_TYPE=branch REF_NAME=main GITHUB_REPOSITORY=example/gcs-mimic
export RUNNER_ARCH=X64 EXPECTED_RUNNER_ARCH=X64 ARCHITECTURE=amd64
export GITHUB_RUN_ID=1 GITHUB_RUN_ATTEMPT=1 RUNNER_TEMP="$work"
GITHUB_SHA=$(git rev-parse HEAD)
export GITHUB_SHA
export GITHUB_OUTPUT="$work/output" GITHUB_ENV="$work/env"

# Execute the committed run block, so a commented-out validator cannot satisfy this test.
extract_step() {
  awk -v step="$2" -v required="$4" '
    function fail(message) { print step ": " message > "/dev/stderr"; exit 1 }
    $0 == "      - name: " step { found = 1; next }
    found && (/^      - / || /^  [^ ]/) { exit }
    !found { next }
    required && /^        (if|continue-on-error):/ { fail("validation must run and fail the job") }
    /^        run:/ {
      if ($0 != "        run: |") fail("expected a literal run block")
      inside = 1; saw_run = 1; next
    }
    inside && /^          / { print substr($0, 11); next }
    inside && /^[[:space:]]*$/ { print ""; next }
    inside { inside = 0 }
    END { if (!found || !saw_run) fail("missing run block") }
  ' "$1" > "$3"
  test -s "$3"
}
extract_step .github/workflows/binaries.yaml 'Verify version and release notes' "$work/validate.sh" 1
extract_step .github/workflows/ghcr.yaml 'Configure native build' "$work/configure.sh" 1
extract_step .github/workflows/binaries.yaml 'Calculate publication flags' "$work/flags.sh" 1
extract_step .github/workflows/binaries.yaml 'Publish verified tag' "$work/publish.sh" 0
cat > "$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  metadata) jq -n --arg version "$VERSION" '{packages:[{name:"gcs-mimic",version:$version}]}' ;;
  xtask)
    [[ "$2 $3" == 'release container-tags' && "$4" == "$VERSION" ]] || exit 1
    printf '%s\n' "$@" > "$WORKFLOW_TEST_ROOT/policy-arguments"
    if [[ ${WORKFLOW_TEST_FAILURE:-} == xtask ]]; then echo 'Mock policy failure' >&2; exit 24; fi
    printf '%s\n' "$WORKFLOW_TEST_TAGS" ;;
  *) exit 90 ;;
esac
SH
cat > "$work/bin/docker" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$WORKFLOW_TEST_ROOT/docker-reads"
SH
cat > "$work/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1 $2" == 'release create' ]]; then
  printf '%s\n' "$@" > "$WORKFLOW_TEST_ROOT/release-arguments"
elif [[ "$1 $2" == api\ repos/*/git/ref/tags/* ]]; then
  jq -n --arg sha "$GITHUB_SHA" '{object:{type:"commit",sha:$sha}}'
elif [[ "$1 $2" == 'api --paginate' ]]; then
  if [[ "$*" == *ltrimstr* ]]; then
    if [[ ${WORKFLOW_TEST_FAILURE:-} == releases ]]; then echo 'Mock release-list failure' >&2; exit 23; fi
    printf '%s\n' "$WORKFLOW_TEST_RELEASES"
  else printf 'v0.17.1\n'; fi
else exit 91; fi
SH
chmod +x "$work/bin/cargo" "$work/bin/docker" "$work/bin/gh"

while IFS= read -r test_case; do
  # The suffix preserves otherwise stripped trailing newlines in malformed versions.
  VERSION=$(jq -r '.[0] + "@"' <<< "$test_case")
  VERSION=${VERSION%@}
  export VERSION RELEASE_TAG="v$VERSION"
  valid=$(jq -r '.[1]' <<< "$test_case")
  helper_result=0
  bash scripts/release-version.sh "$VERSION" > "$work/stage" 2> "$work/error" || helper_result=$?
  if [[ "$valid" == true ]]; then test "$helper_result" = 0
  else test "$helper_result" != 0; test ! -s "$work/stage"; fi
  for step in validate configure; do
    : > "$work/output"
    : > "$work/env"
    : > "$work/docker-reads"
    step_result=0
    bash -euo pipefail "$work/$step.sh" > "$work/stdout" 2> "$work/stderr" || step_result=$?
    if [[ "$valid" == true ]]; then
      test "$step_result" = 0
      if [[ "$step" == validate ]]; then
        grep -Fx "version=$VERSION" "$work/output" > /dev/null
      else
        grep -Fx 'IMAGE=ghcr.io/example/gcs-mimic' "$work/env" > /dev/null
      fi
    else
      test "$step_result" != 0
      test ! -s "$work/output"
      test ! -s "$work/env"
      test ! -s "$work/docker-reads"
    fi
  done
done < <(jq -c '.[]' scripts/release-version-cases.json)

mkdir "$work/scripts"
cp scripts/release-version.sh "$work/scripts/"
printf 'archive.tar.gz\n' > "$work/release-assets"
run_publication() {
  (
    cd "$work"
    bash -euo pipefail "$work/flags.sh" || exit "$?"
    if [[ "$1" == push ]]; then bash -euo pipefail "$work/publish.sh"; fi
  )
}
for scenario in candidate highest_stable older_stable; do
  case "$scenario" in
    candidate) export VERSION=1.0.0-rc.2 WORKFLOW_TEST_TAGS=1.0.0-rc.2; latest=false ;;
    highest_stable) export VERSION=1.0.0 WORKFLOW_TEST_TAGS=$'1.0.0\n1.0\nlatest'; latest=true ;;
    older_stable) export VERSION=0.17.2 WORKFLOW_TEST_TAGS=$'0.17.2\n0.17'; latest=false ;;
  esac
  export RELEASE_TAG="v$VERSION" WORKFLOW_TEST_RELEASES=$'0.17.1\n1.0.0'
  : > "$work/policy-arguments"
  rm -f "$work/release-arguments"
  run_publication dry-run > "$work/stdout"
  test ! -e "$work/release-arguments"
  grep -F -- "--latest=$latest" "$work/stdout" > /dev/null
  run_publication push > "$work/stdout"
  grep -Fx -- "--latest=$latest" "$work/release-arguments" > /dev/null
  grep -Fx -- --verify-tag "$work/release-arguments" > /dev/null
  grep -Fx 1.0.0 "$work/policy-arguments" > /dev/null
  if [[ "$scenario" == candidate ]]; then grep -Fx -- --prerelease "$work/release-arguments" > /dev/null
  elif grep -Fx -- --prerelease "$work/release-arguments"; then exit 1; fi
done
for failure in releases xtask; do
  export WORKFLOW_TEST_FAILURE="$failure"
  rm -f "$work/release-arguments"
  publish_result=0
  run_publication push > "$work/stdout" 2> "$work/stderr" || publish_result=$?
  test "$publish_result" != 0
  test ! -e "$work/release-arguments"
  grep -F 'Mock ' "$work/stderr" > /dev/null
done
echo 'PASS: workflow run blocks enforce shared version cases and explicit stable-only latest promotion'
