#!/usr/bin/env bash
# Each recorded SDK contributes an equal share of 100; historical two-SDK runs
# keep their original weights. Missing or mismatched evidence is never scored.
# A case marked "unsupported" names an operation the SDK itself has no public API
# for; it is excluded from that SDK's denominator and must be marked identically
# for every product, so it can neither raise nor lower one emulator's score.
set -euo pipefail

# The case catalog (sdk-scorecard/cases.json) defines every case: the SDKs that emit it, its area
# and which SDKs mark it unsupported, with the exact reason. A run copies it into its results
# directory; historical directories have none and are validated as before.
# shellcheck disable=SC2016 # A jq program: $root, $case and $s are jq variables, not shell ones.
catalog_errors='. as $root | [
  (if ($root.sdks | type) == "array" and ($root.sdks | length) > 0 and ($root.sdks | unique | length) == ($root.sdks | length)
    then empty else "sdks must list distinct SDK keys" end),
  (if ($root.cases | map(.id) | unique | length) == ($root.cases | length) then empty else "duplicate case IDs" end),
  (($root.notes // {}) | to_entries[] | .key as $sdk |
    if ($root.sdks | index($sdk)) and (.value | type == "string" and length > 0) then empty
    else "invalid SDK note: \($sdk)" end),
  ($root.cases[] | . as $case |
    (if (.id | type == "string" and test("^[a-z0-9_]+\\.[a-z0-9_]+$")) then empty else "invalid ID: \(.id)" end),
    (if ["bucket", "object", "signed", "grpc", "sdk"] | index($case.area) then empty else "invalid area: \(.id)" end),
    (if (.area == "sdk") == (.id | startswith("sdk.")) then empty else "sdk area and sdk. prefix disagree: \(.id)" end),
    (if (.area == "grpc" or .area == "sdk") or .sdks == "all" then empty else "core case not on every SDK: \(.id)" end),
    (if .sdks == "all" or ((.sdks | type) == "array" and (.sdks | length) > 0 and all(.sdks[]; . as $s | $root.sdks | index($s)))
      then empty else "unknown or empty SDK list: \(.id)" end),
    (if (.checks | type) == "array" and (.checks | length) > 0 and all(.checks[]; type == "string" and length > 0)
      then empty else "no checks: \(.id)" end),
    ((.unsupported // {}), (.variants // {}) | to_entries[] | .key as $sdk |
      if ($case.sdks == "all" and ($root.sdks | index($sdk))) or (($case.sdks | type) == "array" and ($case.sdks | index($sdk)))
        then (if .value | type == "string" and length > 0 then empty else "empty reason: \($case.id) \($sdk)" end)
        else "unsupported or variant for an SDK that does not emit it: \($case.id) \($sdk)" end))
]'
if [[ ${1:-} == --check-catalog ]]; then
  catalog=${2:?Usage: bash sdk-scorecard/score.sh --check-catalog CASES_JSON}
  errors=$(jq -r "$catalog_errors | .[]" "$catalog") || { echo "unreadable catalog: $catalog" >&2; exit 1; }
  [[ -z $errors ]] || { echo "$errors" >&2; exit 1; }
  exit 0
fi

dir=${1:?Usage: bash sdk-scorecard/score.sh RESULTS_DIR}
shopt -s nullglob
metas=("$dir"/*.meta.json)
[[ ${#metas[@]} -gt 0 ]] || { echo "no results in $dir" >&2; exit 1; }
sdk_set=$(jq -cS '.sdks // {go: {cases: 57}, ruby: {cases: 51}}' "${metas[0]}")
sdks=()
labels=()
for sdk in go ruby node php python java dotnet rust cpp; do
  if jq -e --arg sdk "$sdk" 'has($sdk)' <<<"$sdk_set" >/dev/null; then
    sdks+=("$sdk")
    case "$sdk" in
      go) labels+=(Go);; ruby) labels+=(Ruby);; node) labels+=(Node.js);; php) labels+=(PHP);; python) labels+=(Python);;
      java) labels+=(Java);; dotnet) labels+=(.NET);; rust) labels+=(Rust);; cpp) labels+=(C++);;
    esac
  fi
done
[[ ${#sdks[@]} -gt 0 && ${#sdks[@]} == "$(jq length <<<"$sdk_set")" ]] || { echo 'invalid SDK set' >&2; exit 1; }
catalog="$dir/cases.json"
if [[ -f $catalog ]]; then
  errors=$(jq -r "$catalog_errors | .[]" "$catalog") || { echo "unreadable catalog: $catalog" >&2; exit 1; }
  [[ -z $errors ]] || { echo "invalid catalog: $errors" >&2; exit 1; }
  [[ $(jq -c '.sdks | sort' "$catalog") == "$(jq -c 'keys' <<<"$sdk_set")" ]] || {
    echo 'catalog SDKs differ from the recorded SDK set' >&2; exit 1;
  }
fi
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
      any(.[]; .unsupported != true) and
      all(.[]; .sdk == $sdk and (.pass | type) == "boolean" and
        (.id | type == "string" and test("^[a-z0-9_]+\\.[a-z0-9_]+$")) and
        (.area == "bucket" or .area == "object" or .area == "signed" or .area == "grpc" or .area == "sdk") and
        ((.area == "sdk") == (.id | startswith("sdk."))) and
        ((has("unsupported") | not) or
          (.unsupported == true and .pass == false and (.detail | type == "string" and length > 0))))
    ' "$file" >/dev/null || { echo "incomplete or invalid results: $file" >&2; exit 1; }
    diff -u <(jq -sc 'map({id, area, unsupported: (.unsupported // false)}) | sort_by(.id)' "$dir/$first.$sdk.jsonl") \
      <(jq -sc 'map({id, area, unsupported: (.unsupported // false)}) | sort_by(.id)' "$file") >&2 || {
        echo "case sets or unsupported markers differ: $file" >&2; exit 1;
      }
    if [[ -f $catalog ]]; then
      # The emitted cases, areas and unsupported reasons must be exactly the catalog's for this SDK.
      # One case per line keeps the diff readable when it fails.
      diff -u <(jq -c --arg sdk "$sdk" '[.cases[] | select(.sdks == "all" or (.sdks | index($sdk))) |
          {id, area, unsupported: (.unsupported[$sdk] // null)}] | sort_by(.id) | .[]' "$catalog") \
        <(jq -sc 'map({id, area, unsupported: (if .unsupported == true then .detail else null end)}) | sort_by(.id) | .[]' "$file") >&2 || {
          echo "cases differ from the catalog: $file" >&2; exit 1;
        }
    fi
  done
done

# The bucket/object/signed core is identical in every SDK so SDKs can be compared. The grpc and
# sdk areas hold each SDK's own transports and idioms; they differ by design, because ordinary
# users of that language reach the emulator through them and a shared core alone misses them.
core='map(select(.area != "grpc" and .area != "sdk") | {id, area}) | sort_by(.id)'
if [[ " ${sdks[*]} " == *' ruby '* ]]; then
  for sdk in "${sdks[@]}"; do
    diff -u <(jq -sc "$core" "$dir/$first.ruby.jsonl") <(jq -sc "$core" "$dir/$first.$sdk.jsonl") >&2 || {
      echo "shared SDK cases differ: $sdk" >&2; exit 1;
    }
  done
fi
# A core-only column appears once SDK-specific cases exist, keeping the shared comparison visible.
has_sdk_cases=false
if cat "$dir/$first".*.jsonl | jq -es 'any(.[]; .area == "sdk")' >/dev/null; then has_sdk_cases=true; fi

echo '# SDK scorecard'
echo
# Each SDK heading repeats under both sections.
echo '<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->'
echo
# A share that one decimal cannot state exactly (100/9) is written as a fraction, so the
# sentence stays true; exact shares keep their historical wording.
weight=$(jq -nr --argjson count "${#sdks[@]}" '(100 / $count) as $share | ($share * 10 | round / 10) as $shown |
  if $shown == $share then "\($share)" else "100/\($count) (about \($shown))" end')
echo "Each SDK contributes $weight points; cases within an SDK have equal weight."
if cat "$dir/$first".*.jsonl | jq -es 'any(.[]; .unsupported == true)' >/dev/null; then
  echo 'Cases the SDK has no public API for are shown as n/a and excluded from that SDK'"'"'s denominator.'
fi
if [[ $has_sdk_cases == true ]]; then
  echo 'Score counts every case, including each SDK'"'"'s own idioms (sdk area) and transports (grpc area).'
  echo 'Core counts only the bucket/object/signed cases shared by every SDK, for cross-SDK comparison.'
fi
echo
header='| Emulator | Version |'
divider='| --- | --- |'
for label in "${labels[@]}"; do header+=" $label |"; divider+=' --- |'; done
if [[ $has_sdk_cases == true ]]; then
  echo "$header Score (/100) | Core (/100) |"
  echo "$divider --- | --- |"
else
  echo "$header Score (/100) |"
  echo "$divider --- |"
fi
# Ratio of passed to scored (supported) cases, optionally restricted to the shared core areas.
ratio='map(select(.unsupported != true)) | {p: map(select(.pass)) | length, t: length}'
for meta in "${metas[@]}"; do
  emulator=$(jq -r .emulator "$meta")
  version=$(jq -r .version "$meta")
  row="| $emulator | \`$version\` |"
  counts=()
  core_counts=()
  for sdk in "${sdks[@]}"; do
    file="$dir/$emulator.$sdk.jsonl"
    count=$(jq -sc "($ratio) + {u: (map(select(.unsupported == true)) | length)}" "$file")
    counts+=("$count")
    core_counts+=("$(jq -sc "map(select(.area != \"grpc\" and .area != \"sdk\")) | $ratio" "$file")")
    row+=" $(jq -r '"\(.p)/\(.t)" + (if .u > 0 then " (\(.u) n/a)" else "" end)' <<<"$count") |"
  done
  score=$(printf '%s\n' "${counts[@]}" | jq -sr 'map(.p / .t) | add / length * 1000 | round / 10')
  if [[ $has_sdk_cases == true ]]; then
    core_score=$(printf '%s\n' "${core_counts[@]}" | jq -sr 'map(.p / .t) | add / length * 1000 | round / 10')
    echo "$row $score | $core_score |"
  else
    echo "$row $score |"
  fi
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
          if $section == "area" then
            map(select(.unsupported != true)) as $scored |
            if ($scored | length) == 0 then "n/a" else "\($scored | map(select(.pass)) | length)/\($scored | length)" end
          elif .[0].unsupported == true then "n/a"
          elif .[0].pass then "✅" else "❌" end
        ' "$dir/$emulator.$sdk.jsonl") |"
      done
      echo "$row"
    done < <(jq -r --arg section "$section" '.[$section]' "$dir/$first.$sdk.jsonl" | awk '!seen[$0]++')
  done
done
