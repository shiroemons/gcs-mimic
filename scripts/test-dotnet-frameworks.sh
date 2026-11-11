#!/usr/bin/env bash
set -euo pipefail

# Checks sdk-tests/dotnet/run.sh against a stub `dotnet`: the .NET SDK suite must run once per
# target framework, on that framework's runtime, and fail when any one run fails.
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
# The stub restores successfully and, for `run --framework F`, prints the runtime line the
# scenario prints, exiting 3 for STUB_FAIL_FRAMEWORK and reporting a .NET 11 runtime for
# STUB_ROLLED_FRAMEWORK, as a roll-forward would.
cat >"$work/bin/dotnet" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >>"$STUB_DOTNET_LOG"
if [[ $1 == restore ]]; then exit 0; fi
test "$1 $2 $3" = "run --no-restore --framework"
framework=$4
major=${framework#net}
major=${major%%.*}
if [[ $framework == "${STUB_ROLLED_FRAMEWORK:-}" ]]; then major=11; fi
echo "dotnet-runtime: .NET $major.0.0"
if [[ $framework == "${STUB_FAIL_FRAMEWORK:-}" ]]; then exit 3; fi
echo 'dotnet-origin-guard: PASS'
SH
chmod +x "$work/bin/dotnet"
export STUB_DOTNET_LOG="$work/dotnet.log"

# Runs the suite script with the stub, storing its exit status in $status and its combined output
# in $work/output.
run_suite() {
  : >"$STUB_DOTNET_LOG"
  status=0
  env PATH="$work/bin:$PATH" bash sdk-tests/dotnet/run.sh --self-check >"$work/output" 2>&1 || status=$?
}
expected_runs() {
  printf '%s\n' 'restore --locked-mode' \
    'run --no-restore --framework net10.0 -- --self-check' \
    'run --no-restore --framework net11.0 -- --self-check'
}

run_suite
test "$status" = 0
expected_runs | diff -u - "$STUB_DOTNET_LOG"
grep -Fx 'PASS: .NET SDK suite on net10.0 and net11.0' "$work/output" >/dev/null

# A failure on either framework fails the suite, and the other framework still runs.
for framework in net10.0 net11.0; do
  STUB_FAIL_FRAMEWORK=$framework run_suite
  test "$status" = 1
  expected_runs | diff -u - "$STUB_DOTNET_LOG"
  grep -Fx "FAIL: .NET SDK suite: $framework exited 3" "$work/output" >/dev/null
done

# A target that ran on another major runtime fails even when its scenario passed.
STUB_ROLLED_FRAMEWORK=net10.0 run_suite
test "$status" = 1
grep -Fx 'FAIL: .NET SDK suite: net10.0 did not run on a .NET 10 runtime' "$work/output" >/dev/null

echo 'PASS: the .NET SDK suite runs on every target framework and fails when any one fails'
