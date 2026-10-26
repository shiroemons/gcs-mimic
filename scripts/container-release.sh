#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
  echo 'Usage: bash scripts/container-release.sh VERSION DIGEST_DIR' >&2
  exit 2
fi
version=$1
digest_dir=$2
: "${IMAGE:?IMAGE is required}" "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}" "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GITHUB_REF_NAME:?GITHUB_REF_NAME is required}"
[[ "$GITHUB_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || exit 1
[[ "$GITHUB_SHA" =~ ^[a-f0-9]{40}$ ]] || exit 1
[[ "$IMAGE" == "ghcr.io/$(printf '%s' "$GITHUB_REPOSITORY" | tr '[:upper:]' '[:lower:]')" ]] || exit 1
[[ "$GITHUB_REF_NAME" == "v$version" ]] || exit 1
[[ -d "$digest_dir" && ! -L "$digest_dir" ]] || exit 1
shopt -s nullglob dotglob
inputs=("$digest_dir"/*)
[[ ${#inputs[@]} == 2 ]] || exit 1
for architecture in amd64 arm64; do
  file="$digest_dir/$architecture.digest"
  [[ -f "$file" && ! -L "$file" ]] || exit 1
  bytes=$(wc -c < "$file")
  [[ $bytes -eq 71 || $bytes -eq 72 ]] || exit 1
  digest=$(cat "$file")
  [[ "$digest" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
done
amd64=$(cat "$digest_dir/amd64.digest")
arm64=$(cat "$digest_dir/arm64.digest")
[[ "$amd64" != "$arm64" ]] || exit 1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo xtask release container-tags "$version" > "$work/initial-tags"

owner=${GITHUB_REPOSITORY%%/*}
name=${IMAGE##*/}
owner_type=$(gh api "repos/$GITHUB_REPOSITORY" --jq '.owner.type')
case "$owner_type" in
  User) namespace=users ;;
  Organization) namespace=orgs ;;
  *) echo 'Unsupported package owner type' >&2; exit 1 ;;
esac
package="$namespace/$owner/packages/container/$name"
visibility=$(gh api "$package" --jq '.visibility')
if [[ "$visibility" != public ]]; then
  echo 'Set this package to Public in Package Settings, then rerun only this failed job.' >&2
  exit 1
fi
# A package exists after the platform pushes; API failures never mean a free tag.
existing=$(gh api --paginate "$package/versions?per_page=100" --jq '.[].metadata.container.tags[]')
releases=$(gh api --paginate "repos/$GITHUB_REPOSITORY/releases?per_page=100" \
  --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name | select(startswith("v")) | ltrimstr("v")')
policy_args=("$version")
full_exists=false
while IFS= read -r tag; do
  if [[ "$tag" == "$version" ]]; then full_exists=true; fi
  if [[ -n "$tag" ]]; then policy_args+=("$tag"); fi
done <<< "$existing"
while IFS= read -r release; do
  if [[ -n "$release" ]]; then policy_args+=("$release"); fi
done <<< "$releases"
tags=$(cargo xtask release container-tags "${policy_args[@]}")
[[ "${tags%%$'\n'*}" == "$version" ]] || exit 1
while IFS= read -r tag; do
  [[ "$tag" == "$version" || "$tag" == "${version%.*}" || "$tag" == latest ]] || exit 1
done <<< "$tags"
assets=$(gh release view "v$version" --repo "$GITHUB_REPOSITORY" --json assets --jq '.assets[].name')
asset_exists=false
while IFS= read -r asset; do
  if [[ "$asset" == CONTAINER-DIGESTS.json ]]; then asset_exists=true; fi
done <<< "$assets"

for architecture in amd64 arm64; do
  digest=$amd64
  if [[ "$architecture" == arm64 ]]; then digest=$arm64; fi
  reference="$IMAGE@$digest"
  docker buildx imagetools inspect --raw "$reference" > "$work/$architecture.json"
  jq -e --arg architecture "$architecture" '
    .schemaVersion == 2 and .mediaType == "application/vnd.oci.image.index.v1+json"
    and (.manifests | type == "array" and length >= 2)
    and ([.manifests[].digest] | all(test("^sha256:[a-f0-9]{64}$")))
    and (([.manifests[].digest] | unique | length) == (.manifests | length))
    and ([.manifests[] | select(.platform.os == "linux" and .platform.architecture == $architecture)] | length == 1)
    and (.manifests as $all
      | [$all[] | select(.platform.os == "linux" and .platform.architecture == $architecture)][0].digest as $runtime
      | all($all[]; .digest == $runtime or
          (.platform.os == "unknown" and .platform.architecture == "unknown"
           and .annotations["vnd.docker.reference.type"] == "attestation-manifest"
           and .annotations["vnd.docker.reference.digest"] == $runtime)))
  ' "$work/$architecture.json" > /dev/null
  docker buildx imagetools inspect --format '{{json .SBOM}}' "$reference" > "$work/$architecture-sbom.json"
  jq -e '.SPDX.spdxVersion | type == "string" and startswith("SPDX-")' "$work/$architecture-sbom.json" > /dev/null
  docker buildx imagetools inspect --format '{{json .Provenance}}' "$reference" > "$work/$architecture-provenance.json"
  # SLSA v0.2 provenance names BuildKit by mobyproject.org; SLSA v1 (BuildKit 0.34)
  # names it by its attestation definitions document.
  jq -e '(.SLSA.buildDefinition.buildType // .SLSA.buildType) | type == "string" and
    (startswith("https://mobyproject.org/buildkit")
      or . == "https://github.com/moby/buildkit/blob/master/docs/attestations/slsa-definitions.md")' \
    "$work/$architecture-provenance.json" > /dev/null
done
jq -S -s '[.[].manifests[]] | unique_by(.digest) | sort_by(.digest)' \
  "$work/amd64.json" "$work/arm64.json" > "$work/expected-descriptors.json"

verify_manifest() {
  docker buildx imagetools inspect --raw "$IMAGE:$version" > "$work/version.json"
  jq -e '.schemaVersion == 2 and .mediaType == "application/vnd.oci.image.index.v1+json"' "$work/version.json" > /dev/null
  jq -S '.manifests | sort_by(.digest)' "$work/version.json" > "$work/actual-descriptors.json"
  if ! cmp -s "$work/expected-descriptors.json" "$work/actual-descriptors.json"; then
    echo 'The immutable version tag does not match the verified platform descriptors' >&2
    exit 1
  fi
  index_digest=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$IMAGE:$version")
  [[ "$index_digest" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
}
write_asset() {
  jq -n --arg image "$IMAGE" --arg version "$version" --arg revision "$GITHUB_SHA" \
    --arg digest "$index_digest" --arg amd64 "$amd64" --arg arm64 "$arm64" \
    --slurpfile amd "$work/amd64.json" --slurpfile arm "$work/arm64.json" '
    {schema_version: 1, image: $image, version: $version, source_commit: $revision,
     index_digest: $digest,
     platform_indexes: {"linux/amd64": $amd64, "linux/arm64": $arm64},
     platform_manifests: {
       "linux/amd64": ($amd[0].manifests[] | select(.platform.os == "linux") | .digest),
       "linux/arm64": ($arm[0].manifests[] | select(.platform.os == "linux") | .digest)},
     attestations: {sbom: "SPDX", provenance: "SLSA", signed: false}}
  ' > "$work/CONTAINER-DIGESTS.json"
}
if [[ "$full_exists" == true ]]; then verify_manifest; fi
if [[ "$asset_exists" == true ]]; then
  [[ "$full_exists" == true ]] || exit 1
  write_asset
  mkdir "$work/existing"
  gh release download "v$version" --repo "$GITHUB_REPOSITORY" \
    --pattern CONTAINER-DIGESTS.json --dir "$work/existing"
  if ! cmp -s "$work/CONTAINER-DIGESTS.json" "$work/existing/CONTAINER-DIGESTS.json"; then
    echo 'The immutable release digest asset differs from this publication' >&2
    exit 1
  fi
fi

# Resolve annotated tags immediately before the first registry or release mutation.
object=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/v$version")
type=$(jq -er '.object.type' <<< "$object")
sha=$(jq -er '.object.sha' <<< "$object")
while [[ "$type" == tag ]]; do
  object=$(gh api "repos/$GITHUB_REPOSITORY/git/tags/$sha")
  type=$(jq -er '.object.type' <<< "$object")
  sha=$(jq -er '.object.sha' <<< "$object")
done
[[ "$type" == commit && "$sha" == "$GITHUB_SHA" ]] || exit 1
if [[ "$full_exists" == false ]]; then
  docker buildx imagetools create --tag "$IMAGE:$version" "$IMAGE@$amd64" "$IMAGE@$arm64"
  verify_manifest
fi
mkdir "$work/anonymous"
DOCKER_CONFIG="$work/anonymous" docker buildx imagetools inspect --format '{{.Manifest.Digest}}' \
  "$IMAGE@$index_digest" > "$work/anonymous-digest"
[[ "$(cat "$work/anonymous-digest")" == "$index_digest" ]] || exit 1
if [[ "$asset_exists" == false ]]; then
  write_asset
  gh release upload "v$version" "$work/CONTAINER-DIGESTS.json" --repo "$GITHUB_REPOSITORY"
fi
while IFS= read -r tag; do
  if [[ "$tag" == "$version" ]]; then continue; fi
  docker buildx imagetools create --tag "$IMAGE:$tag" "$IMAGE@$index_digest"
  actual=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$IMAGE:$tag")
  [[ "$actual" == "$index_digest" ]] || exit 1
done <<< "$tags"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  printf 'Published %s:%s at %s.\n\nTags selected by policy:\n\n%s\n' \
    "$IMAGE" "$version" "$index_digest" "$tags" >> "$GITHUB_STEP_SUMMARY"
fi
printf 'PASS: %s:%s at %s; SPDX SBOM, SLSA provenance and anonymous access verified\n' "$IMAGE" "$version" "$index_digest"
