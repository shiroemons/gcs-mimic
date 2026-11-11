#!/usr/bin/env bash
set -euo pipefail

# Checks scripts/pinned-ruby.sh against stub `ruby` and `mise` commands: the Ruby SDK suite must
# run the Ruby that sdk-tests/ruby/.ruby-version pins, whatever Ruby PATH names first.
cd "$(dirname "$0")/.."
root=$PWD
pinned=$(<sdk-tests/ruby/.ruby-version)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/ruby-bin" "$work/mise-bin" "$work/suite"
cp sdk-tests/ruby/.ruby-version "$work/suite/.ruby-version"
# The stub Ruby reports STUB_RUBY_VERSION, as `ruby -e 'print RUBY_VERSION'` would.
cat >"$work/ruby-bin/ruby" <<'SH'
#!/usr/bin/env bash
printf '%s' "$STUB_RUBY_VERSION"
SH
# The stub mise records its arguments and runs the command with the requested Ruby, unless
# STUB_MISE_KEEPS_RUBY makes it keep the Ruby on PATH, as mise does for a pin that is not
# installed while auto-install is off.
cat >"$work/mise-bin/mise" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "auto-install=${MISE_EXEC_AUTO_INSTALL-unset} $*" >>"$STUB_MISE_LOG"
test "$1" = exec
test "$3" = --
version=${2#ruby@}
shift 3
if [[ -z ${STUB_MISE_KEEPS_RUBY:-} ]]; then export STUB_RUBY_VERSION=$version; fi
exec "$@"
SH
chmod +x "$work/ruby-bin/ruby" "$work/mise-bin/mise"
export STUB_MISE_LOG="$work/mise.log"

# Runs select_pinned_ruby in a fresh shell with the given PATH and Ruby, storing its exit status in
# $status, its stderr in $work/stderr and the selected command prefix in $work/prefix.
run_select() {
  local path=$1 version=$2
  : >"$STUB_MISE_LOG"
  status=0
  # shellcheck disable=SC2016 # The inner shell expands its own arguments and ruby_cmd.
  env PATH="$path" STUB_RUBY_VERSION="$version" bash -c '
    set -euo pipefail
    source "$1/scripts/pinned-ruby.sh"
    select_pinned_ruby "$2"
    for word in ${ruby_cmd[@]+"${ruby_cmd[@]}"}; do printf "%s\n" "$word"; done' bash "$root" "$work/suite" \
    >"$work/prefix" 2>"$work/stderr" || status=$?
}
system_path=/usr/bin:/bin
with_mise="$work/ruby-bin:$work/mise-bin:$system_path"
without_mise="$work/ruby-bin:$system_path"

# The pinned Ruby on PATH is used as it is, without mise.
run_select "$with_mise" "$pinned"
test "$status" = 0
test ! -s "$work/prefix"
test ! -s "$STUB_MISE_LOG"

# Another Ruby on PATH, as mise selects without idiomatic version files: mise runs the pin.
run_select "$with_mise" 3.4.1
test "$status" = 0
printf '%s\n' env MISE_EXEC_AUTO_INSTALL=false mise exec "ruby@$pinned" -- | diff -u - "$work/prefix"
printf '%s\n' "auto-install=false exec ruby@$pinned -- ruby -e print RUBY_VERSION" |
  diff -u - "$STUB_MISE_LOG"

# A pin mise has not installed fails rather than running another Ruby or installing one.
STUB_MISE_KEEPS_RUBY=1 run_select "$with_mise" 3.4.1
test "$status" = 1
grep -F "ran Ruby 3.4.1 instead of the pinned $pinned" "$work/stderr" >/dev/null

# Without mise, another Ruby on PATH fails, naming both versions.
run_select "$without_mise" 3.4.1
test "$status" = 1
grep -F "Ruby 3.4.1 is on PATH, but $work/suite/.ruby-version pins $pinned" "$work/stderr" >/dev/null

# A pin that is not one exact version fails before any Ruby runs.
printf '4.0\n' >"$work/suite/.ruby-version"
run_select "$with_mise" 4.0
test "$status" = 1
grep -F "must hold one exact Ruby version, not '4.0'" "$work/stderr" >/dev/null
test ! -s "$STUB_MISE_LOG"

echo 'PASS: the Ruby SDK suite selects the pinned Ruby from PATH or mise and fails on any other Ruby'
