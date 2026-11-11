#!/usr/bin/env bash
set -euo pipefail

# Lints every tracked Markdown file with the current markdownlint-cli2 release; the release gate
# requires zero issues. Prose follows the root .markdownlint-cli2.jsonc and each file's inline
# directives. The generated feature matrices also get docs/generated-views.markdownlint-cli2.jsonc
# as their base configuration, because their kind sections repeat category headings. A view the
# generator adds later is linted as prose until it is listed here, so it fails visibly rather
# than being skipped. Needs Node.js and npm registry access.
cd "$(dirname "$0")/.."
generated=(docs/en/feature-matrix.md docs/ja/feature-matrix.md)
for file in "${generated[@]}"; do
  git ls-files --error-unmatch -- "$file" >/dev/null || {
    echo "Generated view $file is not tracked" >&2
    exit 1
  }
done
prose=()
while IFS= read -r -d '' file; do
  case " ${generated[*]} " in
    *" $file "*) ;;
    *) prose+=("$file") ;;
  esac
done < <(git ls-files -z -- '*.md')
# The leading colon makes markdownlint-cli2 read each argument as a literal path, not a glob.
prose_status=0
npx --yes markdownlint-cli2 "${prose[@]/#/:}" || prose_status=$?
generated_status=0
npx --yes markdownlint-cli2 --config docs/generated-views.markdownlint-cli2.jsonc \
  "${generated[@]/#/:}" || generated_status=$?
if [[ $prose_status -ne 0 || $generated_status -ne 0 ]]; then
  echo "Markdown lint failed: prose exit $prose_status, generated views exit $generated_status" >&2
  exit 1
fi
echo "PASS: ${#prose[@]} Markdown files and ${#generated[@]} generated views have no lint issues"
