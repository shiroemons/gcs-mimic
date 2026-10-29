#!/usr/bin/env bash
set -euo pipefail

# Every check that reads committed documentation. check.sh runs these with the
# full suite, and the documentation workflow runs only these for changes that
# Foundation checks skips, so a new documentation check belongs here.
cd "$(dirname "$0")/.."
cargo xtask compatibility check
bash scripts/test-release-notes.sh
bash scripts/test-dockerhub-description.sh
