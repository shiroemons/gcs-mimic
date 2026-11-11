#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT \
  DOTNET_ROLL_FORWARD
export DOTNET_CLI_TELEMETRY_OPTOUT=1
# The suite targets the LTS runtime and the pinned SDK's own runtime. Every mode runs once per
# target framework and fails if either run fails or runs on another major runtime, so a missing
# runtime cannot pass by running only the other target.
frameworks=$(sed -n 's:.*<TargetFrameworks>\(.*\)</TargetFrameworks>.*:\1:p' Scenario.csproj)
pattern='^net[0-9]+\.0(;net[0-9]+\.0)+$'
if [[ ! $frameworks =~ $pattern ]]; then
  echo "Scenario.csproj must list two or more TargetFrameworks, not '$frameworks'" >&2
  exit 2
fi
dotnet restore --locked-mode
output=$(mktemp)
trap 'rm -f "$output"' EXIT
failed=()
for framework in ${frameworks//;/ }; do
  major=${framework#net}
  major=${major%%.*}
  echo "== .NET SDK suite on $framework" >&2
  status=0
  dotnet run --no-restore --framework "$framework" -- "$@" | tee "$output" || status=$?
  if [[ $status -ne 0 ]]; then
    failed+=("$framework exited $status")
  elif ! grep -Eq "^dotnet-runtime: \.NET $major\." "$output"; then
    failed+=("$framework did not run on a .NET $major runtime")
  fi
done
if [[ ${#failed[@]} -gt 0 ]]; then
  printf 'FAIL: .NET SDK suite: %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "PASS: .NET SDK suite on ${frameworks//;/ and }"
