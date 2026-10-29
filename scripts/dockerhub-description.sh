#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
  echo 'Usage: bash scripts/dockerhub-description.sh OVERVIEW.md' >&2
  exit 2
fi
overview=$1
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${HUB_USER:?HUB_USER is required}" "${HUB_TOKEN:?HUB_TOKEN is required}"
[[ "$GITHUB_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || exit 1
repository=$(printf '%s' "$GITHUB_REPOSITORY" | tr '[:upper:]' '[:lower:]')

# Docker Hub rejects longer overviews; fail before contacting it.
size=$(wc -c < "$overview")
if (( size == 0 || size > 25000 )); then
  echo "The overview must be 1 to 25000 bytes; $overview has $size" >&2
  exit 1
fi
# Docker Hub resolves no repository-relative links, so each one would be broken.
if grep -oE '\]\([^)]*\)' "$overview" | grep -vE '^\]\((https://|#)' >&2; then
  echo 'Use absolute https:// links in the Docker Hub overview' >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
hub=https://hub.docker.com

# The secret goes through files, never through command-line arguments.
jq -n --arg identifier "$HUB_USER" --arg secret "$HUB_TOKEN" \
  '{identifier:$identifier,secret:$secret}' > "$work/login.json"
curl -fsS --retry 3 -H 'Content-Type: application/json' \
  --data-binary @"$work/login.json" "$hub/v2/auth/token" > "$work/token.json"
access_token=$(jq -er '.access_token | select(type == "string" and . != "")' "$work/token.json")
if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
  echo "::add-mask::$access_token"
fi
printf 'Authorization: Bearer %s\nContent-Type: application/json\n' "$access_token" > "$work/headers"

# The published Hub OpenAPI document has no update route for repositories; this
# is the one peter-evans/dockerhub-description v5.0.0 uses. The anonymous read
# below therefore checks the result instead of trusting the response.
jq -n --rawfile full_description "$overview" '{full_description:$full_description}' > "$work/patch.json"
curl -fsS -X PATCH -H @"$work/headers" \
  --data-binary @"$work/patch.json" "$hub/v2/repositories/$repository" > /dev/null

curl -fsS --retry 3 "$hub/v2/namespaces/${repository%%/*}/repositories/${repository##*/}" \
  > "$work/repository.json"
# Hub may drop trailing whitespace, which Markdown rendering ignores anyway.
if ! jq -e --rawfile expected "$overview" '
  (.full_description // "" | sub("\\s+$"; "")) == ($expected | sub("\\s+$"; ""))' \
  "$work/repository.json" > /dev/null; then
  echo 'Docker Hub does not serve the submitted overview' >&2
  exit 1
fi
echo "PASS: docker.io/$repository overview matches $overview ($size bytes)"
