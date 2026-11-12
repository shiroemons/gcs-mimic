#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
version=$(awk -F'"' '/^\[/ { package = ($0 == "[package]") } package && /^version = / { print $2; exit }' Cargo.toml)
test -n "$version"
for recipe in examples/strict-local/compose.yaml examples/full-stack/compose.yaml; do
  grep -qF "ghcr.io/shiroemons/gcs-mimic:$version}" "$recipe" || {
    echo "$recipe must default to the image of the manifest version $version" >&2
    exit 1
  }
done
# The Go package refuses binaries of another release by this constant, so it moves with the
# manifest version; a stale one would refuse the binary built from the same commit.
module=go/fakestorage/version.go
go_version=$(sed -n 's/^const moduleVersion = "\(.*\)"$/\1/p' "$module")
if [[ "$go_version" != "$version" ]]; then
  echo "$module moduleVersion must equal the manifest version $version" >&2
  exit 1
fi
