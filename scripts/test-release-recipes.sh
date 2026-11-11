#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/scripts" "$work/examples/strict-local" "$work/examples/full-stack"
cp scripts/check-release-recipes.sh "$work/scripts/"
recipes=(examples/strict-local/compose.yaml examples/full-stack/compose.yaml)
write_recipe() {
  printf "image: \${GCSM_IMAGE:-ghcr.io/shiroemons/gcs-mimic:%s}\n" "$2" > "$work/$1"
}
for version in 0.17.1 1.0.0-rc.1 1.0.0; do
  printf '[package]\nversion = "%s"\n' "$version" > "$work/Cargo.toml"
  for recipe in "${recipes[@]}"; do write_recipe "$recipe" "$version"; done
  bash "$work/scripts/check-release-recipes.sh"
  for recipe in "${recipes[@]}"; do
    for other in 0.17.0 1.0.0 1.0.0-rc.1 1.0.0-rc.10; do
      if [[ "$other" == "$version" ]]; then continue; fi
      write_recipe "$recipe" "$other"
      if bash "$work/scripts/check-release-recipes.sh" > "$work/output" 2>&1; then
        echo "A recipe for another release passed: $version $recipe $other" >&2
        exit 1
      fi
      grep -F "$recipe must default to the image of the manifest version $version" "$work/output" > /dev/null
    done
    write_recipe "$recipe" "$version"
  done
done
echo 'PASS: stable, candidate and final release recipes must match the complete manifest version'
