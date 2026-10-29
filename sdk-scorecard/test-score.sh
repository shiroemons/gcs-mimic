#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for product in first second; do
  jq -n --arg product "$product" '{emulator: $product, version: "fixture", sdks:
    {go: {cases: 3}, ruby: {cases: 2}, node: {cases: 2}, php: {cases: 2}, python: {cases: 2}}}' >"$work/$product.meta.json"
  for sdk in go ruby node php python; do
    jq -cn --arg sdk "$sdk" --arg product "$product" '
      {sdk: $sdk, id: "bucket.roundtrip", area: "bucket", pass: true},
      {sdk: $sdk, id: "object.bytes", area: "object", pass: ($product == "first")}' >"$work/$product.$sdk.jsonl"
  done
  jq -cn --arg product "$product" '{sdk: "go", id: "grpc.roundtrip", area: "grpc", pass: ($product == "first")}' >>"$work/$product.go.jsonl"
done
bash "$root/sdk-scorecard/score.sh" "$work" >"$work/output"
grep -Fx "| first | \`fixture\` | 3/3 | 2/2 | 2/2 | 2/2 | 2/2 | 100 |" "$work/output" >/dev/null
grep -Fx "| second | \`fixture\` | 1/3 | 1/2 | 1/2 | 1/2 | 1/2 | 46.7 |" "$work/output" >/dev/null
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
rm "$work/second.php.jsonl"
expect_failure

bash "$root/sdk-scorecard/score.sh" "$root/sdk-scorecard/results/2026-10-29-2" >"$work/legacy"
grep -F '| 24/57 | 24/51 | 44.6 |' "$work/legacy" >/dev/null
grep -F '| 20/57 | 22/51 | 39.1 |' "$work/legacy" >/dev/null
grep -F '| 57/57 | 51/51 | 100 |' "$work/legacy" >/dev/null
echo 'PASS: five-SDK weighting, incomplete/mismatched/invalid evidence rejection and historical two-SDK scoring'
