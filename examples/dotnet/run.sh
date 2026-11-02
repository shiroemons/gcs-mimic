#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# --locked-mode restores from the committed packages.lock.json and never rewrites it.
dotnet restore --locked-mode
exec dotnet run --no-restore
