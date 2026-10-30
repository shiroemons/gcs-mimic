#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin" "$work/tests" "$work/pkg"
export GCS_MIMIC_CHECK_ROOT="$work"
cat >"$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >>"$GCS_MIMIC_CHECK_LOG"
if [[ $* == "${GCS_MIMIC_CHECK_FAIL:-}" ]]; then exit 23; fi
if [[ $* == 'test --workspace --locked --offline --no-run --message-format=json-render-diagnostics' &&
  -z ${GCS_MIMIC_CHECK_NO_TESTS:-} ]]; then
  root=$GCS_MIMIC_CHECK_ROOT
  # A library without a test executable and a non-test example must not be run.
  jq -cn --arg manifest "$root/pkg/Cargo.toml" --arg root "$root" '
    {reason: "compiler-artifact", manifest_path: $manifest, profile: {test: true}, executable: ($root + "/tests/unit-0a1b")},
    {reason: "compiler-artifact", manifest_path: $manifest, profile: {test: false}, executable: null},
    {reason: "compiler-artifact", manifest_path: $manifest, profile: {test: false}, executable: ($root + "/tests/example-2c3d")},
    {reason: "compiler-artifact", manifest_path: $manifest, profile: {test: true}, executable: ($root + "/tests/integration-4e5f")},
    {reason: "build-finished", success: true}'
fi
SH
cat >"$work/tests/binary" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
name=${0##*/}
name=${name%-*}
root=$GCS_MIMIC_CHECK_ROOT
echo "$name $PWD $CARGO_MANIFEST_DIR" >>"$root/ran"
touch "$root/$name.started"
if [[ ${GCS_MIMIC_CHECK_OVERLAP:-} == true ]]; then
  # Neither binary can finish unless both have started; serial execution fails.
  peer=unit
  if [[ $name == unit ]]; then peer=integration; fi
  for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -f $root/$peer.started ]]; then break; fi
    sleep 0.05
  done
  test -f "$root/$peer.started"
fi
if [[ ${GCS_MIMIC_CHECK_FAIL:-} == "binary $name" ]]; then exit 23; fi
echo 'test result: ok. 1 passed; 0 failed'
SH
for name in unit-0a1b integration-4e5f example-2c3d; do cp "$work/tests/binary" "$work/tests/$name"; done
cat >"$work/bin/rustc" <<'SH'
#!/usr/bin/env bash
echo 'host: aarch64-apple-darwin'
SH
cat >"$work/bin/rustup" <<'SH'
#!/usr/bin/env bash
printf '%s\n' x86_64-pc-windows-msvc x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin
SH
chmod +x "$work/bin/cargo" "$work/bin/rustc" "$work/bin/rustup" "$work"/tests/*
export PATH="$work/bin:$PATH"
export GCS_MIMIC_CHECK_LOG="$work/actual"
unset GCSM_TEST_JOBS
cat >"$work/expected" <<'TEXT'
fetch --locked
fmt --all --check
clippy --workspace --all-targets --locked --offline -- -D warnings
xtask lint
clippy --workspace --all-targets --locked --offline --target x86_64-pc-windows-msvc -- -D warnings
xtask lint --target x86_64-pc-windows-msvc
clippy --workspace --all-targets --locked --offline --target x86_64-unknown-linux-gnu -- -D warnings
xtask lint --target x86_64-unknown-linux-gnu
clippy --workspace --all-targets --locked --offline --target x86_64-unknown-linux-musl -- -D warnings
xtask lint --target x86_64-unknown-linux-musl
build --locked --offline
test --workspace --locked --offline --no-run --message-format=json-render-diagnostics
test --workspace --locked --offline --doc
xtask compatibility check
TEXT
printf '%s\n' "integration $work/pkg $work/pkg" "unit $work/pkg $work/pkg" >"$work/expected-ran"

# Runs scripts/check.sh with fresh binary records, storing its exit status in $status.
run_check() {
  : >"$work/actual"
  : >"$work/ran"
  rm -f "$work"/*.started
  status=0
  bash scripts/check.sh >"$work/output" 2>&1 || status=$?
}

GCS_MIMIC_CHECK_FAIL='' run_check
test "$status" = 0
diff -u "$work/expected" "$work/actual"
sort "$work/ran" | diff -u "$work/expected-ran" -
grep -F 'PASS: 2 test binaries and doctests with 1 concurrent binaries' "$work/output" >/dev/null

GCS_MIMIC_CHECK_FAIL='' GCS_MIMIC_CHECK_OVERLAP=true GCSM_TEST_JOBS=2 run_check
test "$status" = 0
sort "$work/ran" | diff -u "$work/expected-ran" -

: >"$work/prefix"
while IFS= read -r command; do
  printf '%s\n' "$command" >>"$work/prefix"
  if [[ $command == 'test --workspace --locked --offline --doc' ]]; then
    # A failing test binary still lets the other binaries and the doctest lane finish.
    for name in unit integration; do
      GCS_MIMIC_CHECK_FAIL="binary $name" run_check
      test "$status" = 23
      diff -u "$work/prefix" "$work/actual"
      sort "$work/ran" | diff -u "$work/expected-ran" -
      grep -Fx "FAIL $name (exit 23)" "$work/output" >/dev/null
    done
  fi
  GCS_MIMIC_CHECK_FAIL="$command" run_check
  if [[ $status == 0 ]]; then
    echo "Checks continued after failure: $command" >&2
    exit 1
  fi
  test "$status" = 23
  diff -u "$work/prefix" "$work/actual"
done <"$work/expected"

no_tests_prefix=$(sed -n '1,/--no-run/p' "$work/expected")
GCS_MIMIC_CHECK_FAIL='' GCS_MIMIC_CHECK_NO_TESTS=1 run_check
test "$status" = 1
diff -u <(printf '%s\n' "$no_tests_prefix") "$work/actual"
grep -F 'reported no test executables' "$work/output" >/dev/null

for jobs in 0 -1 x; do
  status=0
  GCSM_TEST_JOBS=$jobs bash scripts/test-rust.sh >"$work/output" 2>&1 || status=$?
  test "$status" = 2
done

# The RAM disk wrapper preserves the command's arguments and status when it is disabled.
status=0
GCSM_TEST_RAMDISK=0 TMPDIR="$work/tmp" bash scripts/with-test-tmpdir.sh \
  bash -c 'test "$TMPDIR|$1|${GCSM_TEST_JOBS:-unset}" = "$0|two words|unset"; exit 7' "$work/tmp" 'two words' ||
  status=$?
test "$status" = 7
for setting in 'GCSM_TEST_RAMDISK=2' 'GCSM_TEST_RAMDISK_MB=0'; do
  status=0
  env "$setting" bash scripts/with-test-tmpdir.sh true >"$work/output" 2>&1 || status=$?
  if [[ $(uname -s) == Darwin || $setting == GCSM_TEST_RAMDISK=2 ]]; then test "$status" = 2; fi
done
status=0
bash scripts/with-test-tmpdir.sh >"$work/output" 2>&1 || status=$?
test "$status" = 2
echo 'PASS: shared checks run in order, run every test binary, and stop at every failing command'
