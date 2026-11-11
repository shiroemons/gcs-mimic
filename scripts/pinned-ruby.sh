# shellcheck shell=bash
# Sourced by scripts/sdk-go.sh before it builds anything for the Ruby SDK suite.
#
# Bundler refuses any Ruby other than the one sdk-tests/ruby/.ruby-version pins (exit 18), but the
# Ruby that `ruby` names depends on the developer's version manager. rbenv, chruby and asdf read
# .ruby-version; mise does so only when idiomatic version files are enabled for Ruby, and otherwise
# runs its global default, which is how a full pre-push once failed late on Ruby 3.4.1. The Ruby on
# PATH is kept when it is already the pin; otherwise mise, when installed, runs the exact pin. Like
# the rest of the SDK harness this installs nothing: with auto-install off, mise falls back to the
# Ruby on PATH when the pin is missing, and the version check below rejects it. Any other Ruby
# fails before the servers start, naming both versions.

# Sets the array ruby_cmd to the command prefix that runs the pinned Ruby and its Bundler from the
# given directory, or fails when no such Ruby can be selected.
# Arguments: the directory that holds .ruby-version and the Gemfile.
select_pinned_ruby() {
  local dir=$1 pinned actual
  pinned=$(<"$dir/.ruby-version")
  if [[ ! $pinned =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "$dir/.ruby-version must hold one exact Ruby version, not '$pinned'" >&2
    return 1
  fi
  ruby_cmd=()
  actual=$(cd "$dir" && ruby -e 'print RUBY_VERSION' 2>/dev/null) || actual=none
  if [[ $actual == "$pinned" ]]; then return 0; fi
  if command -v mise >/dev/null 2>&1; then
    ruby_cmd=(env MISE_EXEC_AUTO_INSTALL=false mise exec "ruby@$pinned" --)
    actual=$(cd "$dir" && "${ruby_cmd[@]}" ruby -e 'print RUBY_VERSION') || actual=none
    if [[ $actual == "$pinned" ]]; then return 0; fi
    echo "mise exec ruby@$pinned ran Ruby $actual instead of the pinned $pinned; install it with: mise install ruby@$pinned" >&2
    return 1
  fi
  echo "Ruby $actual is on PATH, but $dir/.ruby-version pins $pinned; install Ruby $pinned with a version manager that reads .ruby-version, or install mise" >&2
  return 1
}
