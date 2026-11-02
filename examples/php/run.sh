#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

composer install --no-interaction --no-scripts --no-progress --quiet
php main.php
