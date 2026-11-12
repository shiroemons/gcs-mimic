#!/usr/bin/env bash
set -euo pipefail

# Every check that reads committed documentation. check.sh runs these with the
# full suite, and the documentation workflow runs only these for changes that
# Foundation checks skips, so a new documentation check belongs here.
cd "$(dirname "$0")/.."
# Setting is the only source of accepted server environment names. The exceptions are
# deliberately rejected inputs retained in migration diagnostics and negative tests.
# The explicit history baseline preserves original execution records, not current instructions.
# shortcut: per-file counts cannot detect a replacement at the same count; use line fingerprints if needed.
git grep -n -oE 'GCS_MIMIC_[A-Z0-9_]+' | awk -F: '
  BEGIN {
    count = split("FOO_BAR UPPER_SNAKE_CASE NO_SUCH_SETTING UNKNOWN_SETTING ENDPOINT GRPC_ENDPOINT SYNC_TOKEN BINARY PORT IMAGE TLS_DIR", names, " ")
    for (i = 1; i <= count; i++) allowed["GCS_MIMIC_" names[i]] = 1
  }
  FILENAME == ARGV[1] { allowed[$0] = 1; next }
  FILENAME == ARGV[2] {
    if ($0 !~ /^#/ && NF) {
      split($0, record, "\t")
      historical[record[1] SUBSEP "GCS_MIMIC_" record[2]] = record[3]
    }
    next
  }
  !($NF in allowed) {
    key = $1 SUBSEP $NF
    if (historical[key] > 0) {
      historical[key]--
      preserved++
      next
    }
    print $0 ": non-setting variables must use GCSM_" > "/dev/stderr"
    errors++
  }
  END {
    if (errors) {
      print "Environment namespace check: " errors " invalid occurrences" > "/dev/stderr"
      exit 1
    }
    print "Environment namespace check: PASS"
    print "Historical environment records: " preserved + 0 " occurrences preserved by scripts/environment-history-baseline.txt"
  }' <(sed -n '/enum Setting {/,/^    }/p' src/config/setting.rs |
    awk -F '"' '/=>/ { print "GCS_MIMIC_" toupper($2) }') scripts/environment-history-baseline.txt -
cargo xtask compatibility check
# The option lists in --help, docs/en/native-binary.md and llms.txt are compared with the
# settings the server accepts. A documentation-only change runs no other test, so these run here.
cargo test --locked --lib -- config::help::tests
# The strict local recipe's README shows the block to paste, and the container smoke test runs
# its compose.yaml, so the block must be that file byte for byte.
awk '/^<!-- strict-local compose.yaml -->$/ { marked = 1; next }
  marked && !inside && /^```yaml$/ { inside = 1; next }
  inside && /^```$/ { exit }
  inside' examples/strict-local/README.md | cmp - examples/strict-local/compose.yaml || {
  echo 'examples/strict-local/README.md must show examples/strict-local/compose.yaml unchanged' >&2
  exit 1
}
# Recipes run with their own release's image, including an exact RC tag. This is separate from
# floating tags: the README quick start keeps latest on stable releases. Smoke tests override it.
bash scripts/check-release-recipes.sh
bash scripts/test-release-recipes.sh
bash scripts/test-release-notes.sh
bash scripts/test-dockerhub-description.sh
