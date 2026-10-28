#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
  echo 'Usage: bash scripts/container-mirror.sh VERSION' >&2
  exit 2
fi
version=$1
: "${SOURCE_IMAGE:?SOURCE_IMAGE is required}" "${IMAGE:?IMAGE is required}" "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
[[ "$GITHUB_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || exit 1
repository=$(printf '%s' "$GITHUB_REPOSITORY" | tr '[:upper:]' '[:lower:]')
[[ "$SOURCE_IMAGE" == "ghcr.io/$repository" ]] || exit 1
[[ "$IMAGE" == "docker.io/$repository" ]] || exit 1
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || exit 1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
hub="https://hub.docker.com/v2/namespaces/${repository%%/*}/repositories/${repository##*/}"

# A first push would otherwise create the repository with the account's default
# visibility, and anonymous Hub API reads cannot see private repositories.
if ! curl -fsS --retry 3 "$hub" > "$work/repository.json" \
  || ! jq -e '.is_private == false' "$work/repository.json" > /dev/null; then
  echo 'Create a public Docker Hub repository with this name, then rerun only this failed job.' >&2
  exit 1
fi

# The GitHub Release digest report is the record of what GHCR publication verified.
gh release download "v$version" --repo "$GITHUB_REPOSITORY" \
  --pattern CONTAINER-DIGESTS.json --dir "$work"
index_digest=$(jq -er --arg image "$SOURCE_IMAGE" --arg version "$version" '
  select(.schema_version == 1 and .image == $image and .version == $version)
  | .index_digest | select(test("^sha256:[a-f0-9]{64}$"))' "$work/CONTAINER-DIGESTS.json")
source_commit=$(jq -er '.source_commit | select(test("^[a-f0-9]{40}$"))' "$work/CONTAINER-DIGESTS.json")
object=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/v$version")
type=$(jq -er '.object.type' <<< "$object")
sha=$(jq -er '.object.sha' <<< "$object")
while [[ "$type" == tag ]]; do
  object=$(gh api "repos/$GITHUB_REPOSITORY/git/tags/$sha")
  type=$(jq -er '.object.type' <<< "$object")
  sha=$(jq -er '.object.sha' <<< "$object")
done
[[ "$type" == commit && "$sha" == "$source_commit" ]] || exit 1
source_digest=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$SOURCE_IMAGE:$version")
if [[ "$source_digest" != "$index_digest" ]]; then
  echo 'The GHCR version tag does not match the release digest report' >&2
  exit 1
fi

verify_attestations() {
  docker buildx imagetools inspect --format '{{json .SBOM}}' "$1" > "$work/sbom.json"
  jq -e '(keys == ["linux/amd64", "linux/arm64"])
    and all(.[]; .SPDX.spdxVersion | type == "string" and startswith("SPDX-"))' \
    "$work/sbom.json" > /dev/null
  docker buildx imagetools inspect --format '{{json .Provenance}}' "$1" > "$work/provenance.json"
  # Accept the same SLSA v0.2 and v1 BuildKit predicates as GHCR publication.
  jq -e '(keys == ["linux/amd64", "linux/arm64"])
    and all(.[]; (.SLSA.buildDefinition.buildType // .SLSA.buildType) | type == "string" and
      (startswith("https://mobyproject.org/buildkit")
        or . == "https://github.com/moby/buildkit/blob/master/docs/attestations/slsa-definitions.md"))' \
    "$work/provenance.json" > /dev/null
}
verify_attestations "$SOURCE_IMAGE@$index_digest"

# API failures never mean a free tag.
existing=()
next="$hub/tags?page_size=100"
while [[ "$next" != null ]]; do
  [[ "$next" == "$hub/tags?"* ]] || exit 1
  curl -fsS --retry 3 "$next" > "$work/tags.json"
  names=$(jq -r '.results | if type == "array" then .[].name else error("missing tag results") end' "$work/tags.json")
  while IFS= read -r tag; do
    if [[ -n "$tag" ]]; then existing+=("$tag"); fi
  done <<< "$names"
  next=$(jq -r '.next // "null"' "$work/tags.json")
done
releases=$(gh api --paginate "repos/$GITHUB_REPOSITORY/releases?per_page=100" \
  --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name | select(startswith("v")) | ltrimstr("v")')
policy_args=("$version")
full_exists=false
for tag in ${existing[@]+"${existing[@]}"}; do
  if [[ "$tag" == "$version" ]]; then full_exists=true; fi
  policy_args+=("$tag")
done
while IFS= read -r release; do
  if [[ -n "$release" ]]; then policy_args+=("$release"); fi
done <<< "$releases"
tags=$(cargo xtask release container-tags "${policy_args[@]}")
[[ "${tags%%$'\n'*}" == "$version" ]] || exit 1
while IFS= read -r tag; do
  [[ "$tag" == "$version" || "$tag" == "${version%.*}" || "$tag" == latest ]] || exit 1
done <<< "$tags"
if [[ "$full_exists" == true ]]; then
  actual=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$IMAGE:$version")
  if [[ "$actual" != "$index_digest" ]]; then
    echo 'The immutable version tag on Docker Hub does not match the GHCR index' >&2
    exit 1
  fi
fi

# A single-source copy keeps the index bytes, so both registries share one digest.
if [[ "$full_exists" == false ]]; then
  docker buildx imagetools create --tag "$IMAGE:$version" "$SOURCE_IMAGE@$index_digest"
  actual=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$IMAGE:$version")
  [[ "$actual" == "$index_digest" ]] || exit 1
fi
verify_attestations "$IMAGE@$index_digest"
mkdir "$work/anonymous"
DOCKER_CONFIG="$work/anonymous" docker buildx imagetools inspect --format '{{.Manifest.Digest}}' \
  "$IMAGE@$index_digest" > "$work/anonymous-digest"
[[ "$(cat "$work/anonymous-digest")" == "$index_digest" ]] || exit 1
while IFS= read -r tag; do
  if [[ "$tag" == "$version" ]]; then continue; fi
  docker buildx imagetools create --tag "$IMAGE:$tag" "$IMAGE@$index_digest"
  actual=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$IMAGE:$tag")
  [[ "$actual" == "$index_digest" ]] || exit 1
done <<< "$tags"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  printf 'Mirrored %s:%s to %s at %s.\n\nTags selected by policy:\n\n%s\n' \
    "$SOURCE_IMAGE" "$version" "$IMAGE" "$index_digest" "$tags" >> "$GITHUB_STEP_SUMMARY"
fi
printf 'PASS: %s:%s at %s; SPDX SBOM, SLSA provenance and anonymous access verified\n' "$IMAGE" "$version" "$index_digest"
