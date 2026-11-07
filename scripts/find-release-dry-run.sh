#!/usr/bin/env bash
# Prints the run whose binaries a tag of COMMIT publishes: the newest successful manual run of the
# release workflow on that commit that still holds every archive. A rehearsal also succeeds but
# holds none, and artifacts expire, so success alone does not name such a run.
set -euo pipefail

if [[ $# != 1 ]]; then
  echo 'Usage: bash scripts/find-release-dry-run.sh COMMIT' >&2
  exit 2
fi
commit=$1
repository=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY names the repository}
# The build matrix of .github/workflows/binaries.yaml makes this many.
archives=8

runs=$(gh run list --repo "$repository" --workflow binaries.yaml --commit "$commit" \
  --event workflow_dispatch --status success --limit 20 --json databaseId --jq '.[].databaseId')
for run in $runs; do
  held=$(gh api "repos/$repository/actions/runs/$run/artifacts?per_page=100" \
    --jq '[.artifacts[] | select(.expired | not) | select(.name | startswith("binary-"))] | length')
  if [[ $held == "$archives" ]]; then
    echo "$run"
    exit 0
  fi
done
echo "No successful dry run of $commit holds its $archives archives. Run Release binaries manually on this commit, then re-run this job." >&2
exit 1
