#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT
export DOTNET_CLI_TELEMETRY_OPTOUT=1
dotnet restore --locked-mode
exec dotnet run --no-restore -- "$@"
