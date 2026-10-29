#!/usr/bin/env bash
# Scores recorded scorecard results: each SDK contributes 50 points, split
# equally across its cases, so a product passing every Go and Ruby case scores 100.
set -euo pipefail

dir=${1:?Usage: bash sdk-scorecard/score.sh RESULTS_DIR}
shopt -s nullglob
metas=("$dir"/*.meta.json)
[[ ${#metas[@]} -gt 0 ]] || { echo "no results in $dir" >&2; exit 1; }

echo "# SDK scorecard"
echo
echo '<!-- markdownlint-configure-file {"MD013": false} -->'
echo
echo "| Emulator | Version | Go | Ruby | Score (/100) |"
echo "| --- | --- | --- | --- | --- |"
for meta in "${metas[@]}"; do
  emulator=$(jq -r .emulator "$meta")
  version=$(jq -r .version "$meta")
  go=$(jq -s '{p: map(select(.pass)) | length, t: length}' "$dir/$emulator.go.jsonl")
  rb=$(jq -s '{p: map(select(.pass)) | length, t: length}' "$dir/$emulator.ruby.jsonl")
  jq -nr --arg e "$emulator" --arg v "$version" --argjson go "$go" --argjson rb "$rb" '
    def half(r): if r.t == 0 then 0 else 50 * r.p / r.t end;
    "| \($e) | `\($v)` | \($go.p)/\($go.t) | \($rb.p)/\($rb.t) | \((half($go) + half($rb)) * 10 | round / 10) |"'
done

echo
echo "## Passed cases by area"
echo
header="| SDK | Area |"
divider="| --- | --- |"
for meta in "${metas[@]}"; do
  header+=" $(jq -r .emulator "$meta") |"
  divider+=" --- |"
done
echo "$header"
echo "$divider"
first=$(jq -r .emulator "${metas[0]}")
for sdk in go ruby; do
  jq -r '.area' "$dir/$first.$sdk.jsonl" | awk '!seen[$0]++' | while read -r area; do
    row="| $sdk | $area |"
    for meta in "${metas[@]}"; do
      emulator=$(jq -r .emulator "$meta")
      row+=" $(jq -rs --arg a "$area" 'map(select(.area == $a)) | "\(map(select(.pass)) | length)/\(length)"' "$dir/$emulator.$sdk.jsonl") |"
    done
    echo "$row"
  done
done

echo
echo "## Case results"
echo
header="| SDK | Case |"
divider="| --- | --- |"
for meta in "${metas[@]}"; do
  header+=" $(jq -r .emulator "$meta") |"
  divider+=" --- |"
done
echo "$header"
echo "$divider"
first=$(jq -r .emulator "${metas[0]}")
for sdk in go ruby; do
  jq -r '.id' "$dir/$first.$sdk.jsonl" | while read -r id; do
    row="| $sdk | \`$id\` |"
    for meta in "${metas[@]}"; do
      emulator=$(jq -r .emulator "$meta")
      row+=" $(jq -r --arg id "$id" 'select(.id == $id) | if .pass then "✅" else "❌" end' "$dir/$emulator.$sdk.jsonl") |"
    done
    echo "$row"
  done
done
