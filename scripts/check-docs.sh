#!/usr/bin/env bash
set -euo pipefail

# Every check that reads committed documentation. check.sh runs these with the
# full suite, and the documentation workflow runs only these for changes that
# Foundation checks skips, so a new documentation check belongs here.
cd "$(dirname "$0")/.."
cargo xtask compatibility check
# The strict local recipe's README shows the block to paste, and the container smoke test runs
# its compose.yaml, so the block must be that file byte for byte.
awk '/^<!-- strict-local compose.yaml -->$/ { marked = 1; next }
  marked && !inside && /^```yaml$/ { inside = 1; next }
  inside && /^```$/ { exit }
  inside' examples/strict-local/README.md | cmp - examples/strict-local/compose.yaml || {
  echo 'examples/strict-local/README.md must show examples/strict-local/compose.yaml unchanged' >&2
  exit 1
}
# These recipes use settings that only their own release has, and a release's tag publishes them
# as they are. The container smoke test sets GCS_MIMIC_IMAGE, so it never reads their default.
version=$(awk -F'"' '/^\[/ { package = ($0 == "[package]") } package && /^version = / { print $2; exit }' Cargo.toml)
for recipe in examples/strict-local/compose.yaml examples/full-stack/compose.yaml; do
  grep -qF "ghcr.io/shiroemons/gcs-mimic:$version}" "$recipe" || {
    echo "$recipe must default to the image of the manifest version $version" >&2
    exit 1
  }
done
bash scripts/test-release-notes.sh
bash scripts/test-dockerhub-description.sh
