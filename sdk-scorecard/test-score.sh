#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

sdks=(go ruby node php python java dotnet rust)
for product in first second; do
  jq -n --arg product "$product" '{emulator: $product, version: "fixture", sdks:
    {go: {cases: 4}, ruby: {cases: 3}, node: {cases: 3}, php: {cases: 3}, python: {cases: 3},
     java: {cases: 3}, dotnet: {cases: 3}, rust: {cases: 3}}}' >"$work/$product.meta.json"
  for sdk in "${sdks[@]}"; do
    # The Rust fixture has no API for HMAC keys, so that shared case is unsupported there.
    jq -cn --arg sdk "$sdk" --arg product "$product" '
      {sdk: $sdk, id: "bucket.roundtrip", area: "bucket", pass: true},
      {sdk: $sdk, id: "object.bytes", area: "object", pass: ($product == "first")},
      if $sdk == "rust" then
        {sdk: $sdk, id: "project.hmac_keys", area: "bucket", pass: false, unsupported: true, detail: "no public API"}
      else {sdk: $sdk, id: "project.hmac_keys", area: "bucket", pass: true} end' >"$work/$product.$sdk.jsonl"
  done
  jq -cn --arg product "$product" '{sdk: "go", id: "grpc.roundtrip", area: "grpc", pass: ($product == "first")}' >>"$work/$product.go.jsonl"
done
bash "$root/sdk-scorecard/score.sh" "$work" >"$work/output"
grep -Fx "| first | \`fixture\` | 4/4 | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | 2/2 (1 n/a) | 100 |" "$work/output" >/dev/null
# (2/4 + 6 × 2/3 + 1/2) / 8: the unsupported Rust case neither passes nor counts.
grep -Fx "| second | \`fixture\` | 2/4 | 2/3 | 2/3 | 2/3 | 2/3 | 2/3 | 2/3 | 1/2 (1 n/a) | 62.5 |" "$work/output" >/dev/null
test "$(grep -cFx '### Node.js' "$work/output")" = 2
test "$(grep -cFx '### .NET' "$work/output")" = 2
grep -Fx '| grpc | 1/1 | 0/1 |' "$work/output" >/dev/null
grep -Fx "| \`grpc.roundtrip\` | ✅ | ❌ |" "$work/output" >/dev/null
grep -Fx "| \`project.hmac_keys\` | n/a | n/a |" "$work/output" >/dev/null
grep -F 'excluded from that SDK' "$work/output" >/dev/null
# Rust's bucket area counts only the supported round trip.
awk '/^### Rust$/ { rust = 1 } rust && /^\| bucket \|/ { print; exit }' "$work/output" | grep -Fx '| bucket | 1/1 | 1/1 |' >/dev/null
cp "$work/second.node.jsonl" "$work/saved"
expect_failure() {
  if bash "$root/sdk-scorecard/score.sh" "$work" >"$work/output" 2>"$work/error"; then
    echo 'invalid evidence unexpectedly scored' >&2; exit 1
  fi
  test ! -s "$work/output"
}
: >"$work/second.node.jsonl"
expect_failure
jq -c 'if .id == "object.bytes" then .id = "object.other" else . end' "$work/saved" >"$work/second.node.jsonl"
expect_failure
jq -c '.pass = "true"' "$work/saved" >"$work/second.node.jsonl"
expect_failure
jq -c '.sdk = "ruby"' "$work/saved" >"$work/second.node.jsonl"
expect_failure
cp "$work/saved" "$work/second.node.jsonl"
jq '.transport = {scheme: "http"}' "$work/second.meta.json" >"$work/changed"
mv "$work/changed" "$work/second.meta.json"
expect_failure
jq 'del(.transport)' "$work/second.meta.json" >"$work/changed"
mv "$work/changed" "$work/second.meta.json"
# Unsupported markers must agree across products and cannot carry a pass or hide every case.
cp "$work/second.rust.jsonl" "$work/saved-rust"
cp "$work/first.rust.jsonl" "$work/saved-first-rust"
for change in 'del(.unsupported, .detail)' '.pass = true' 'del(.detail)' '.unsupported = "yes"'; do
  jq -c "if .id == \"project.hmac_keys\" then $change else . end" "$work/saved-rust" >"$work/second.rust.jsonl"
  expect_failure
done
jq -c 'if .id == "object.bytes" then . + {pass: false, unsupported: true, detail: "no public API"} else . end' \
  "$work/saved-rust" >"$work/second.rust.jsonl"
expect_failure
for product in first second; do
  jq -c '. + {pass: false, unsupported: true, detail: "no public API"}' "$work/saved-rust" >"$work/$product.rust.jsonl"
done
expect_failure
cp "$work/saved-rust" "$work/second.rust.jsonl"
cp "$work/saved-first-rust" "$work/first.rust.jsonl"
bash "$root/sdk-scorecard/score.sh" "$work" >/dev/null
rm "$work/second.php.jsonl"
expect_failure

bash "$root/sdk-scorecard/score.sh" "$root/sdk-scorecard/results/2026-10-29-2" >"$work/legacy"
grep -F '| 24/57 | 24/51 | 44.6 |' "$work/legacy" >/dev/null
grep -F '| 20/57 | 22/51 | 39.1 |' "$work/legacy" >/dev/null
grep -F '| 57/57 | 51/51 | 100 |' "$work/legacy" >/dev/null
# SDK-specific idioms may exist in only some SDKs; the shared core still has to match.
idioms="$work/idioms"
mkdir "$idioms"
for product in first second; do
  jq -n --arg product "$product" '{emulator: $product, version: "fixture", sdks: {ruby: {cases: 1}, java: {cases: 3}}}' \
    >"$idioms/$product.meta.json"
  jq -cn '{sdk: "ruby", id: "bucket.roundtrip", area: "bucket", pass: true}' >"$idioms/$product.ruby.jsonl"
  jq -cn --arg product "$product" '
    {sdk: "java", id: "bucket.roundtrip", area: "bucket", pass: true},
    {sdk: "java", id: "sdk.transfer_manager_upload", area: "sdk", pass: ($product == "first")},
    {sdk: "java", id: "sdk.storage_batch", area: "sdk", pass: false}' >"$idioms/$product.java.jsonl"
done
bash "$root/sdk-scorecard/score.sh" "$idioms" >"$work/output"
grep -F '| Score (/100) | Core (/100) |' "$work/output" >/dev/null
# first: (1/1 + 2/3) / 2 = 83.3 overall, core 100; second: (1/1 + 1/3) / 2 = 66.7, core 100.
grep -Fx "| first | \`fixture\` | 1/1 | 2/3 | 83.3 | 100 |" "$work/output" >/dev/null
grep -Fx "| second | \`fixture\` | 1/1 | 1/3 | 66.7 | 100 |" "$work/output" >/dev/null
grep -Fx '| sdk | 1/2 | 0/2 |' "$work/output" >/dev/null
cp "$idioms/second.java.jsonl" "$work/saved-java"
for change in '.area = "object"' '.id = "object.storage_batch"'; do
  jq -c "if .id == \"sdk.storage_batch\" then $change else . end" "$work/saved-java" >"$idioms/second.java.jsonl"
  if bash "$root/sdk-scorecard/score.sh" "$idioms" >/dev/null 2>&1; then
    echo "mislabelled SDK-specific case unexpectedly scored: $change" >&2; exit 1
  fi
done
cp "$work/saved-java" "$idioms/second.java.jsonl"
# A core case missing from one SDK is still rejected even when that SDK adds idioms.
jq -c 'select(.id != "bucket.roundtrip")' "$work/saved-java" >"$idioms/first.java.jsonl"
cp "$idioms/first.java.jsonl" "$idioms/second.java.jsonl"
jq '.sdks.java.cases = 2' "$idioms/first.meta.json" >"$work/changed" && mv "$work/changed" "$idioms/first.meta.json"
jq '.sdks.java.cases = 2' "$idioms/second.meta.json" >"$work/changed" && mv "$work/changed" "$idioms/second.meta.json"
if bash "$root/sdk-scorecard/score.sh" "$idioms" >/dev/null 2>&1; then
  echo 'SDK without the shared core unexpectedly scored' >&2; exit 1
fi

bash "$root/sdk-scorecard/score.sh" "$root/sdk-scorecard/results/2026-10-29-5" >"$work/five"
cmp "$work/five" "$root/sdk-scorecard/results/2026-10-29-5/summary.md"
echo 'PASS: eight-SDK weighting, unsupported-case exclusion, SDK-specific idioms with a shared core, incomplete/mismatched/invalid evidence rejection and historical scoring'
