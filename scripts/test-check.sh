#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
cat >"$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >>"$GCS_MIMIC_CHECK_LOG"
if [[ $* == "${GCS_MIMIC_CHECK_FAIL:-}" ]]; then exit 23; fi
SH
cat >"$work/bin/rustc" <<'SH'
#!/usr/bin/env bash
echo 'host: aarch64-apple-darwin'
SH
cat >"$work/bin/rustup" <<'SH'
#!/usr/bin/env bash
printf '%s\n' x86_64-pc-windows-msvc x86_64-unknown-linux-musl aarch64-apple-darwin
SH
chmod +x "$work/bin/cargo" "$work/bin/rustc" "$work/bin/rustup"
export PATH="$work/bin:$PATH"
export GCS_MIMIC_CHECK_LOG="$work/actual"
cat >"$work/expected" <<'TEXT'
fetch --locked
fmt --all --check
clippy --workspace --all-targets --locked --offline -- -D warnings
clippy --workspace --all-targets --locked --offline --target x86_64-pc-windows-msvc -- -D warnings
clippy --workspace --all-targets --locked --offline --target x86_64-unknown-linux-musl -- -D warnings
test --workspace --locked --offline
xtask compatibility check
TEXT

GCS_MIMIC_CHECK_FAIL='' bash scripts/check.sh >"$work/output" 2>&1
diff -u "$work/expected" "$work/actual"
while IFS= read -r command; do
  printf '%s\n' "$command" >>"$work/prefix"
  : >"$work/actual"
  if GCS_MIMIC_CHECK_FAIL="$command" bash scripts/check.sh >"$work/output" 2>&1; then
    echo "Checks continued after failure: $command" >&2
    exit 1
  else
    test "$?" = 23
  fi
  diff -u "$work/prefix" "$work/actual"
done <"$work/expected"
echo 'PASS: shared checks run in order and stop at every failing Cargo command'
