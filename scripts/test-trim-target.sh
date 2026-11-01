#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/repo/scripts"
cp scripts/trim-target.sh "$work/repo/scripts/"
cat >"$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
echo "$*" >>"$TRIM_CARGO_LOG"
SH
chmod +x "$work/bin/cargo"
export PATH="$work/bin:$PATH" TRIM_CARGO_LOG="$work/cargo.log"
target=$work/repo/target
trim() { bash "$work/repo/scripts/trim-target.sh" 2>>"$work/stderr"; }
fail() { echo "trim-target: $*" >&2; exit 1; }

# Session contents are written before the crate directories' times are set, since adding an
# entry advances the parent's modification time just as a rustc session does.
crate() {
  mkdir -p "$1/s-session"
  dd if=/dev/zero of="$1/s-session/query-cache.bin" bs=1024 count="${3:-1}" 2>/dev/null
  touch -t "$2" "$1"
}
reset() {
  rm -rf "$target" "$work/cargo.log" "$work/stderr"
  # A fixed past time stays portable; `date -v` and `date -d` differ between BSD and GNU.
  old=202001010000
  crate "$target/debug/incremental/gcs_mimic-old" "$old"
  crate "$target/debug/incremental/gcs_mimic-new" "$(date +%Y%m%d%H%M)"
  crate "$target/cross/x86_64-pc-windows-msvc/debug/incremental/xtask-old" "$old"
  mkdir -p "$target/debug/deps"
}

# Without a target directory there is nothing to trim.
trim
[[ ! -e $target && ! -e $work/cargo.log ]] || fail 'acted without a target directory'

# Only crate directories untouched for longer than the window go, in every incremental root.
reset
trim
[[ ! -e $target/debug/incremental/gcs_mimic-old ]] || fail 'kept a stale host crate directory'
[[ ! -e $target/cross/x86_64-pc-windows-msvc/debug/incremental/xtask-old ]] ||
  fail 'kept a stale cross crate directory'
[[ -d $target/debug/incremental/gcs_mimic-new/s-session ]] || fail 'removed a current crate directory'
[[ ! -e $work/cargo.log ]] || fail 'cleaned build outputs below the size limit'
grep -q 'removed 2 stale incremental crate directories$' "$work/stderr" || fail 'did not report the pruning'

# A window of zero days keeps a directory modified today.
reset
GCSM_TARGET_INCREMENTAL_DAYS=0 trim
[[ -d $target/debug/incremental/gcs_mimic-new ]] || fail 'a zero-day window removed a current directory'

# Over the limit, all incremental state goes before any build output.
reset
crate "$target/debug/incremental/gcs_mimic-new" "$(date +%Y%m%d%H%M)" 2048
GCSM_TARGET_MAX_MB=1 trim
[[ ! -e $target/debug/incremental ]] || fail 'kept incremental state over the limit'
[[ ! -e $work/cargo.log ]] || fail 'cleaned build outputs although dropping incremental state sufficed'
grep -q 'and all incremental state$' "$work/stderr" || fail 'did not report dropping incremental state'

# Still over the limit, the debug and cross-check outputs are cleaned through Cargo.
reset
dd if=/dev/zero of="$target/debug/deps/libbig.rlib" bs=1024 count=2048 2>/dev/null
GCSM_TARGET_MAX_MB=1 trim
printf '%s\n' 'clean --profile dev' 'clean --target-dir target/cross' | cmp -s - "$work/cargo.log" ||
  fail "unexpected Cargo calls: $(cat "$work/cargo.log" 2>/dev/null)"

# Invalid settings are rejected before anything is deleted.
reset
for setting in GCSM_TARGET_INCREMENTAL_DAYS=x GCSM_TARGET_MAX_MB=0 GCSM_TARGET_MAX_MB=-1; do
  status=0
  env "$setting" bash "$work/repo/scripts/trim-target.sh" 2>/dev/null || status=$?
  [[ $status == 2 ]] || fail "accepted $setting (exit $status)"
done
[[ -e $target/debug/incremental/gcs_mimic-old ]] || fail 'deleted after rejecting a setting'

echo 'PASS: trim-target keeps current state and bounds target/'
