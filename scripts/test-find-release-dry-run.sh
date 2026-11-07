#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
# Answers from GH_RUNS (run IDs, newest first) and GH_HELD (`run:archives` pairs).
cat >"$work/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >>"$GH_LOG"
if [[ $1 == run ]]; then
  printf '%s\n' $GH_RUNS
  exit 0
fi
run=${2#repos/owner/name/actions/runs/}
run=${run%%/*}
for pair in $GH_HELD; do
  if [[ ${pair%:*} == "$run" ]]; then echo "${pair#*:}"; exit 0; fi
done
echo 0
SH
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH" GITHUB_REPOSITORY=owner/name GH_LOG="$work/log"

find_run() {
  status=0
  : >"$GH_LOG"
  bash scripts/find-release-dry-run.sh abc123 >"$work/out" 2>"$work/err" || status=$?
}

# The newest run that holds every archive wins over a rehearsal and over a partly expired run.
GH_RUNS='30 20 10 5' GH_HELD='30:0 20:7 10:8 5:8' find_run
test "$status" = 0
test "$(cat "$work/out")" = 10
grep -F -- '--commit abc123' "$GH_LOG" >/dev/null
grep -F -- '--event workflow_dispatch' "$GH_LOG" >/dev/null
grep -F -- '--status success' "$GH_LOG" >/dev/null
if grep -F 'runs/5/' "$GH_LOG" >/dev/null; then
  echo 'Kept looking after finding a complete dry run' >&2
  exit 1
fi

# Nothing to publish stops the tag run and says what to do.
for runs in '' '30 20'; do
  GH_RUNS=$runs GH_HELD='30:0 20:7' find_run
  test "$status" = 1
  test ! -s "$work/out"
  grep -F 'No successful dry run of abc123 holds its 8 archives' "$work/err" >/dev/null
done

status=0
bash scripts/find-release-dry-run.sh >/dev/null 2>&1 || status=$?
test "$status" = 2
echo 'PASS: a tag finds the newest dry run holding every archive and stops without one'
