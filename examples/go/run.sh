#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
# -mod=readonly uses the committed go.sum and never rewrites it.
go run -mod=readonly .
