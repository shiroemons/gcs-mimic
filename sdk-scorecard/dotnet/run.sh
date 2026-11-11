#!/usr/bin/env bash
# Build and run the official .NET SDK probe.
#   run.sh build                     restore from packages.lock.json and compile only
#   run.sh [--transport-check]       build, then run
#   run.sh --no-build [--transport-check]   run the existing Release build
set -euo pipefail
cd "$(dirname "$0")"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1

build() {
  dotnet restore --locked-mode >&2
  dotnet build --no-restore -c Release -nologo -v quiet >&2
}

case "${1:-}" in
  build) build; exit ;;
  --no-build) shift ;;
  *) build ;;
esac
exec dotnet bin/Release/net11.0/Probe.dll "$@"
