#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

BUNDLE_FROZEN=true bundle install --quiet
bundle exec ruby main.rb
