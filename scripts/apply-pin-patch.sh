#!/usr/bin/env bash
# Applies a pin refresh patch to the index, or fails if it does anything else. The patch comes from
# a job that compiled dependencies, so it is untrusted input here: a path list could not stop it
# from changing code that CI then runs on the proposal branch. Every changed file must therefore
# differ from HEAD only in digest and snapshot values, and the evidence record may only grow.
# Usage: apply-pin-patch.sh PATCH
set -euo pipefail

patch=$1
evidence=docs/en/evidence/dependency-refresh.md

reject() {
  echo "Rejected pin patch: $1" >&2
  exit 1
}

normalize() {
  sed -E 's/sha256:[0-9a-f]{64}/sha256:<digest>/g; s/(snapshot=|"snapshot": ")[0-9]{8}T[0-9]{6}Z/\1<snapshot>/g'
}

# --summary reports every creation, deletion, rename, copy and mode change.
summary=$(git apply --summary "$patch")
[[ -z $summary ]] || reject "it changes the file set or modes: $summary"
git apply --index "$patch"

while IFS= read -r -d '' path; do
  if [[ $path == "$evidence" ]]; then
    size=$(git cat-file -s "HEAD:$path")
    cmp -s -n "$size" <(git show "HEAD:$path") "$path" || reject "$path is not append-only"
  elif ! cmp -s <(git show "HEAD:$path" | normalize) <(normalize <"$path"); then
    reject "$path changes more than digest and snapshot values"
  fi
done < <(git diff --cached --no-ext-diff --name-only -z)
