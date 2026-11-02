#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

dotnet restore --locked-mode
exec dotnet run --no-restore
