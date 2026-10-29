#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
  echo 'Usage: bash scripts/release-notes.sh REPOSITORY-RELATIVE-NOTES.md REF' >&2
  exit 2
fi
notes=$1
ref=$2
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
[[ "$GITHUB_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || exit 1
[[ "$ref" =~ ^[A-Za-z0-9_.-]+$ ]] || exit 1
# Links resolve against the notes' place in the repository, which an absolute
# or parent-relative path would not identify.
[[ "$notes" != /* && "$notes" != ../* && "$notes" != */../* ]] || exit 1
cd "$(dirname "$0")/.."
test -s "$notes"

# A GitHub Release body resolves no repository-relative links, so each one is
# rewritten to the file at the released ref. Targets must exist in this
# checkout, so a broken link fails here instead of on the published Release.
base=$(dirname "$notes")
awk -v base="$base" -v prefix="https://github.com/$GITHUB_REPOSITORY/blob/$ref/" '
function rewrite(target,   anchor, at, path, parts, count, depth, i, kept, resolved) {
  if (target ~ /^(https?:\/\/|mailto:|#)/) return target
  at = index(target, "#")
  anchor = at ? substr(target, at) : ""
  path = at ? substr(target, 1, at - 1) : target
  count = split(base "/" path, parts, "/")
  depth = 0
  for (i = 1; i <= count; i++) {
    if (parts[i] == "" || parts[i] == ".") continue
    if (parts[i] == "..") {
      if (depth == 0) { fail("leaves the repository", target); return target }
      depth--
      continue
    }
    kept[++depth] = parts[i]
  }
  resolved = ""
  for (i = 1; i <= depth; i++) resolved = resolved (i > 1 ? "/" : "") kept[i]
  if (resolved == "" || system("test -e \"" resolved "\"") != 0) {
    fail("has no target in the repository", target)
    return target
  }
  return prefix resolved anchor
}
function fail(reason, target) {
  printf "%s:%d: link %s: %s\n", FILENAME, FNR, reason, target > "/dev/stderr"
  failed = 1
}
# Fenced code is literal text, so links inside it are left alone.
/^ *(```|~~~)/ { fenced = !fenced; print; next }
fenced { print; next }
match($0, /^ {0,3}\[[^]]+\]: */) {
  rest = substr($0, RLENGTH + 1)
  split(rest, words, " ")
  print substr($0, 1, RLENGTH) rewrite(words[1]) substr(rest, length(words[1]) + 1)
  next
}
{
  line = $0
  out = ""
  while (match(line, /\]\([^) ]+\)/)) {
    out = out substr(line, 1, RSTART - 1) "](" rewrite(substr(line, RSTART + 2, RLENGTH - 3)) ")"
    line = substr(line, RSTART + RLENGTH)
  }
  print out line
}
END { exit failed }
' "$notes"
