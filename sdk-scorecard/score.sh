#!/usr/bin/env bash
# Each recorded SDK contributes an equal share of 100; historical two-SDK runs
# keep their original weights. Missing or mismatched evidence is never scored.
set -euo pipefail

dir=${1:?Usage: bash sdk-scorecard/score.sh RESULTS_DIR}
shopt -s nullglob
metas=("$dir"/*.meta.json)
[[ ${#metas[@]} -gt 0 ]] || { echo "no results in $dir" >&2; exit 1; }
sdk_set=$(jq -cS '.sdks // {go: {cases: 57}, ruby: {cases: 51}}' "${metas[0]}")
sdks=()
labels=()
for sdk in go ruby node php python; do
  if jq -e --arg sdk "$sdk" 'has($sdk)' <<<"$sdk_set" >/dev/null; then
    sdks+=("$sdk")
    case "$sdk" in go) labels+=(Go);; ruby) labels+=(Ruby);; node) labels+=(Node.js);; php) labels+=(PHP);; python) labels+=(Python);; esac
  fi
done
[[ ${#sdks[@]} -gt 0 && ${#sdks[@]} == "$(jq length <<<"$sdk_set")" ]] || { echo 'invalid SDK set' >&2; exit 1; }
first=$(jq -er '.emulator' "${metas[0]}")
transport=$(jq -cS '.transport // null' "${metas[0]}")
for meta in "${metas[@]}"; do
  emulator=$(jq -er .emulator "$meta")
  [[ $emulator =~ ^[a-z0-9-]+$ ]] || { echo 'invalid emulator name' >&2; exit 1; }
  [[ $(jq -cS '.transport // null' "$meta") == "$transport" ]] || {
    echo "transport inputs differ: $emulator" >&2; exit 1;
  }
  [[ $(jq -cS '.sdks // {go: {cases: 57}, ruby: {cases: 51}}' "$meta") == "$sdk_set" ]] || {
    echo "SDK inputs differ: $emulator" >&2; exit 1;
  }
  for sdk in "${sdks[@]}"; do
    file="$dir/$emulator.$sdk.jsonl"
    expected=$(jq -er --arg sdk "$sdk" '.[$sdk].cases | select(type == "number" and . > 0 and . == floor)' <<<"$sdk_set")
    jq -es --arg sdk "$sdk" --argjson expected "$expected" '
      length == $expected and (map(.id) | unique | length) == $expected and
      all(.[]; .sdk == $sdk and (.pass | type) == "boolean" and
        (.id | type == "string" and test("^[a-z0-9_]+\\.[a-z0-9_]+$")) and
        (.area == "bucket" or .area == "object" or .area == "signed" or .area == "grpc"))
    ' "$file" >/dev/null || { echo "incomplete or invalid results: $file" >&2; exit 1; }
    diff -u <(jq -sc 'map({id, area}) | sort_by(.id)' "$dir/$first.$sdk.jsonl") \
      <(jq -sc 'map({id, area}) | sort_by(.id)' "$file") >&2 || { echo "case sets differ: $file" >&2; exit 1; }
  done
done

if [[ " ${sdks[*]} " == *' ruby '* ]]; then
  for sdk in "${sdks[@]}"; do
    diff -u <(jq -sc 'map(select(.area != "grpc") | {id, area}) | sort_by(.id)' "$dir/$first.ruby.jsonl") \
      <(jq -sc 'map(select(.area != "grpc") | {id, area}) | sort_by(.id)' "$dir/$first.$sdk.jsonl") >&2 || {
        echo "shared SDK cases differ: $sdk" >&2; exit 1;
      }
  done
fi

echo '# SDK scorecard'
echo
# Each SDK heading repeats under both sections.
echo '<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->'
echo
echo "Each SDK contributes $(jq -nr --argjson count "${#sdks[@]}" '100 / $count') points; cases within an SDK have equal weight."
echo
header='| Emulator | Version |'
divider='| --- | --- |'
for label in "${labels[@]}"; do header+=" $label |"; divider+=' --- |'; done
echo "$header Score (/100) |"
echo "$divider --- |"
for meta in "${metas[@]}"; do
  emulator=$(jq -r .emulator "$meta")
  version=$(jq -r .version "$meta")
  row="| $emulator | \`$version\` |"
  counts=()
  for sdk in "${sdks[@]}"; do
    count=$(jq -sc '{p: map(select(.pass)) | length, t: length}' "$dir/$emulator.$sdk.jsonl")
    counts+=("$count")
    row+=" $(jq -r '"\(.p)/\(.t)"' <<<"$count") |"
  done
  score=$(printf '%s\n' "${counts[@]}" | jq -sr 'map(.p / .t) | add / length * 1000 | round / 10')
  echo "$row $score |"
done

for section in area id; do
  echo
  if [[ $section == area ]]; then echo '## Passed cases by area'; title=Area; else echo '## Case results'; title=Case; fi
  header="| $title |"
  divider='| --- |'
  for meta in "${metas[@]}"; do header+=" $(jq -r .emulator "$meta") |"; divider+=' --- |'; done
  for index in "${!sdks[@]}"; do
    sdk=${sdks[$index]}
    echo
    echo "### ${labels[$index]}"
    echo
    echo "$header"
    echo "$divider"
    while IFS= read -r value; do
      if [[ $section == id ]]; then row="| \`$value\` |"; else row="| $value |"; fi
      for meta in "${metas[@]}"; do
        emulator=$(jq -r .emulator "$meta")
        row+=" $(jq -rs --arg section "$section" --arg value "$value" '
          map(select(.[$section] == $value)) |
          if $section == "area" then "\(map(select(.pass)) | length)/\(length)"
          else if .[0].pass then "✅" else "❌" end end
        ' "$dir/$emulator.$sdk.jsonl") |"
      done
      echo "$row"
    done < <(jq -r --arg section "$section" '.[$section]' "$dir/$first.$sdk.jsonl" | awk '!seen[$0]++')
  done
done
