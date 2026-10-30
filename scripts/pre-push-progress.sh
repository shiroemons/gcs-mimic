#!/usr/bin/env bash
# Reports `just pre-push` progress. The Justfile calls it between stages:
#   start                 begins the run and a background reporter
#   check|check-sdk|...   marks that stage finished and the next one started
# Estimates come from the stage durations of earlier runs, kept in
# target/pre-push-progress/history; without history the percentage counts stages only.
# GCSM_PRE_PUSH_PROGRESS_INTERVAL sets the reporter period in seconds (default 30, 0 disables it).
set -euo pipefail

stages=(check check-sdk check-containers)
labels=('Rust and shell checks' 'SDK suites' 'container checks')
mark=${1:?Usage: bash scripts/pre-push-progress.sh MARK JUST_PID}
just_pid=${2:?Usage: bash scripts/pre-push-progress.sh MARK JUST_PID}
interval=${GCSM_PRE_PUSH_PROGRESS_INTERVAL:-30}
if [[ ! $interval =~ ^[0-9]+$ ]]; then
  echo 'GCSM_PRE_PUSH_PROGRESS_INTERVAL must be a non-negative integer' >&2
  exit 2
fi
dir=target/pre-push-progress
state=$dir/state
history=$dir/history
# Estimates read the history as it was when the run started, so a stage that this run
# just recorded cannot replace the earlier duration the remaining estimate relies on.
baseline=$dir/baseline
mkdir -p "$dir"

duration() {
  local seconds=$1
  if ((seconds >= 60)); then printf '%dm%02ds' $((seconds / 60)) $((seconds % 60)); else printf '%ds' "$seconds"; fi
}

# The last recorded duration of a stage, or nothing.
estimate() {
  [[ -f $baseline ]] || return 0
  awk -v stage="$1" '$1 == stage { value = $2 } END { if (value != "") print value }' "$baseline"
}

# Prints "<percent> <remaining seconds or ->" for stage index $1 that has run $2 seconds.
overall() {
  local index=$1 elapsed=$2 total=0 done=0 remaining=0 known=true i value
  for i in "${!stages[@]}"; do
    value=$(estimate "${stages[i]}")
    if [[ -z $value ]]; then known=false; break; fi
    total=$((total + value))
    if ((i < index)); then
      done=$((done + value))
    elif ((i == index)); then
      if ((elapsed < value)); then
        done=$((done + elapsed))
        remaining=$((remaining + value - elapsed))
      else
        done=$((done + value))
      fi
    else
      remaining=$((remaining + value))
    fi
  done
  if [[ $known == true ]]; then
    # Stages that all finished within a second leave no time to weigh, so count stages.
    local percent=$((index * 100 / ${#stages[@]}))
    if ((total > 0)); then percent=$((done * 100 / total)); fi
    # A running stage never reports completion before it finishes.
    if ((index < ${#stages[@]} && percent > 99)); then percent=99; fi
    echo "$percent $remaining"
  else
    echo "$((index * 100 / ${#stages[@]})) -"
  fi
}

report() {
  local index=$1 started=$2 now elapsed percent remaining stage_eta
  now=$(date +%s)
  elapsed=$((now - started))
  read -r percent remaining < <(overall "$index" "$elapsed")
  stage_eta=$(estimate "${stages[index]}")
  local line="[pre-push $((index + 1))/${#stages[@]} ${percent}%] ${labels[index]} running for $(duration "$elapsed")"
  if [[ -n $stage_eta ]]; then line+=" (last run took $(duration "$stage_eta"))"; fi
  if [[ $remaining != - ]]; then line+="; about $(duration "$remaining") left overall"; fi
  echo "$line"
}

# Background reporter: prints the running stage every $interval seconds until the run
# finishes or the just process that owns it exits.
reporter() {
  local last
  last=$(date +%s)
  while kill -0 "$just_pid" 2>/dev/null && [[ -f $state ]]; do
    sleep 1
    [[ -f $state ]] || break
    if (($(date +%s) - last >= interval)); then
      local index started owner
      read -r owner index started <"$state" || break
      [[ $owner == "$just_pid" ]] || break
      report "$index" "$started" || true
      last=$(date +%s)
    fi
  done
}

now=$(date +%s)
if [[ $mark == start ]]; then
  if [[ -f $history ]]; then cp "$history" "$baseline"; else rm -f "$baseline"; fi
  echo "$just_pid 0 $now" >"$state"
  echo "$now" >"$dir/run-start"
  read -r percent remaining < <(overall 0 0)
  plan="[pre-push 1/${#stages[@]} ${percent}%] ${labels[0]} started"
  if [[ $remaining != - ]]; then
    plan+="; about $(duration "$remaining") in total based on the last run"
  else
    plan+="; no earlier run recorded, so time estimates start after this run"
  fi
  echo "$plan"
  if ((interval > 0)); then
    reporter </dev/null &
    disown
  fi
  exit 0
fi

index=-1
for i in "${!stages[@]}"; do
  if [[ ${stages[i]} == "$mark" ]]; then index=$i; fi
done
if ((index < 0)); then echo "Unknown pre-push stage: $mark" >&2; exit 2; fi
read -r owner current started <"$state"
if [[ $owner != "$just_pid" || $current != "$index" ]]; then
  echo "pre-push progress state does not match stage $mark" >&2
  exit 2
fi
took=$((now - started))
printf '%s\t%s\n' "$mark" "$took" >>"$history"
next=$((index + 1))
if ((next < ${#stages[@]})); then
  echo "$just_pid $next $now" >"$state"
  read -r percent remaining < <(overall "$next" 0)
  line="[pre-push $((index + 1))/${#stages[@]} done] ${labels[index]} took $(duration "$took")."
  line+=" [pre-push $((next + 1))/${#stages[@]} ${percent}%] ${labels[next]} started"
  if [[ $remaining != - ]]; then line+="; about $(duration "$remaining") left"; fi
  echo "$line"
else
  rm -f "$state"
  total=$((now - $(cat "$dir/run-start")))
  echo "[pre-push ${#stages[@]}/${#stages[@]} 100%] ${labels[index]} took $(duration "$took"); all stages passed in $(duration "$total")"
fi
