#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
script="$PWD/scripts/dockerhub-description.sh"
overview="$PWD/distribution/dockerhub-overview.md"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
export DESCRIPTION_TEST_ROOT="$work"
export PATH="$work/bin:$PATH"
export GITHUB_REPOSITORY=Example/gcs-mimic HUB_USER=example
export HUB_TOKEN=mock-secret-do-not-print DESCRIPTION_TEST_ACCESS=mock-access-do-not-print
unset GITHUB_ACTIONS

cat > "$work/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$DESCRIPTION_TEST_ROOT/curl-arguments"
method=GET
while [[ $# -gt 1 ]]; do
  case "$1" in
    -X) method=$2; shift ;;
    --data-binary) data=${2#@}; shift ;;
    -H) if [[ "$2" == @* ]]; then headers=${2#@}; fi; shift ;;
  esac
  shift
done
url=$1
if [[ "$method" == GET && -n "${data:-}" ]]; then method=POST; fi
printf '%s %s\n' "$method" "$url" >> "$DESCRIPTION_TEST_ROOT/calls"
if [[ "$DESCRIPTION_TEST_FAIL" == "$method" ]]; then
  echo 'Mock Docker Hub API failure' >&2
  exit 22
fi
case "$method $url" in
  'POST https://hub.docker.com/v2/auth/token')
    jq -e --arg secret "$HUB_TOKEN" '. == {identifier:"example",secret:$secret}' "$data" > /dev/null
    jq -n --arg token "$DESCRIPTION_TEST_RETURNED" '{access_token:$token}' ;;
  'PATCH https://hub.docker.com/v2/repositories/example/gcs-mimic')
    grep -Fx "Authorization: Bearer $DESCRIPTION_TEST_ACCESS" "$headers" > /dev/null
    cp "$data" "$DESCRIPTION_TEST_ROOT/patch.json"
    echo '{}' ;;
  'GET https://hub.docker.com/v2/namespaces/example/repositories/gcs-mimic')
    if [[ "$DESCRIPTION_TEST_SERVED" == stale ]]; then
      echo '{"full_description":"An older overview"}'
    else
      # Mimic Hub dropping the trailing newline that the source file keeps.
      jq '{full_description:(.full_description | rtrimstr("\n"))}' "$DESCRIPTION_TEST_ROOT/patch.json"
    fi ;;
  *) echo 'Unexpected mock curl request' >&2; exit 90 ;;
esac
SH
chmod +x "$work/bin/curl"

reset_case() {
  : > "$work/calls"
  : > "$work/curl-arguments"
  rm -f "$work/patch.json"
  export DESCRIPTION_TEST_FAIL='' DESCRIPTION_TEST_SERVED=submitted
  export DESCRIPTION_TEST_RETURNED="$DESCRIPTION_TEST_ACCESS"
}
run_case() {
  local result=0
  bash "$script" "${1:-$overview}" > "$work/output" 2>&1 || result=$?
  if grep -F -e "$HUB_TOKEN" -e "$DESCRIPTION_TEST_ACCESS" "$work/output" "$work/curl-arguments"; then
    echo 'A Docker Hub secret was exposed' >&2
    exit 1
  fi
  return "$result"
}
fail_offline() {
  if run_case "$@"; then echo 'Invalid overview unexpectedly succeeded' >&2; exit 1; fi
  test ! -s "$work/calls"
}

reset_case
: > "$work/empty.md"
fail_offline "$work/empty.md"
reset_case
head -c 25001 /dev/zero | tr '\0' a > "$work/large.md"
fail_offline "$work/large.md"
grep -F 'must be 1 to 25000 bytes' "$work/output" > /dev/null
for link in '[guide](docs/en/getting-started.md)' '[site](http://example.com)' '![logo](logo.png)'; do
  reset_case
  printf '# Overview\n\nSee %s.\n' "$link" > "$work/relative.md"
  fail_offline "$work/relative.md"
  grep -F 'Use absolute https:// links' "$work/output" > /dev/null
done

reset_case
export DESCRIPTION_TEST_FAIL=POST
if run_case; then echo 'Authentication failure unexpectedly succeeded' >&2; exit 1; fi
test ! -e "$work/patch.json"
reset_case
export DESCRIPTION_TEST_RETURNED=''
if run_case; then echo 'An empty access token unexpectedly succeeded' >&2; exit 1; fi
test ! -e "$work/patch.json"
reset_case
export DESCRIPTION_TEST_FAIL=PATCH
if run_case; then echo 'Update failure unexpectedly succeeded' >&2; exit 1; fi
if grep -F 'GET ' "$work/calls"; then exit 1; fi
reset_case
export DESCRIPTION_TEST_SERVED=stale
if run_case; then echo 'A stale overview unexpectedly passed' >&2; exit 1; fi
grep -F 'does not serve the submitted overview' "$work/output" > /dev/null

reset_case
run_case
jq -e --rawfile expected "$overview" '. == {full_description:$expected}' "$work/patch.json" > /dev/null
test "$(cut -d ' ' -f 1 "$work/calls" | paste -sd ' ' -)" = 'POST PATCH GET'
grep -F 'PASS: docker.io/example/gcs-mimic overview matches' "$work/output" > /dev/null
echo 'PASS: Docker Hub overview sync checks size, links, authentication, update and served content without network access'
