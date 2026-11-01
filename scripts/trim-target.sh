#!/usr/bin/env bash
# Bounds the local target directory so incremental compilation can stay enabled.
# Cargo never deletes incremental state: each compiler, flag or feature change starts new
# `incremental/<crate>-<hash>` directories and leaves the old ones behind, which grew target/
# to tens of gigabytes. A crate directory's modification time advances whenever rustc starts a
# session in it, so one untouched for GCSM_TARGET_INCREMENTAL_DAYS (default 3) belongs to a
# configuration no recent build used; deleting it costs at most one non-incremental compile.
# If target/ still exceeds GCSM_TARGET_MAX_MB (default 30720), all incremental state goes, and
# then the debug and cross-check build outputs. Run it while no Cargo build uses target/.
set -euo pipefail

cd "$(dirname "$0")/.."
days=${GCSM_TARGET_INCREMENTAL_DAYS:-3}
max_mb=${GCSM_TARGET_MAX_MB:-30720}
if [[ ! $days =~ ^[0-9]+$ ]]; then
  echo 'GCSM_TARGET_INCREMENTAL_DAYS must be a non-negative integer' >&2
  exit 2
fi
if [[ ! $max_mb =~ ^[1-9][0-9]*$ ]]; then
  echo 'GCSM_TARGET_MAX_MB must be a positive integer' >&2
  exit 2
fi
if [[ ! -d target ]]; then exit 0; fi

size_kb() { du -sk target | cut -f 1; }
incremental_roots() { find target -mindepth 2 -maxdepth 4 -type d -name incremental -prune; }

before=$(size_kb)
pruned=0
while IFS= read -r root; do
  while IFS= read -r crate; do
    rm -r "$crate"
    pruned=$((pruned + 1))
  done < <(find "$root" -mindepth 1 -maxdepth 1 -type d -mtime "+$days")
done < <(incremental_roots)

limit_kb=$((max_mb * 1024))
step=
if (($(size_kb) > limit_kb)); then
  step='all incremental state'
  while IFS= read -r root; do rm -r "$root"; done < <(incremental_roots)
fi
if (($(size_kb) > limit_kb)); then
  step='debug and cross-check build outputs'
  cargo clean --profile dev
  cargo clean --target-dir target/cross
fi

after=$(size_kb)
if ((pruned > 0)) || [[ -n $step ]]; then
  echo "trim-target: $((before / 1024)) MiB -> $((after / 1024)) MiB; removed $pruned stale incremental crate directories${step:+ and $step}" >&2
fi
