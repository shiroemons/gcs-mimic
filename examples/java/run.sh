#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Maven has no native lockfile: fail if the resolved graph differs from dependencies.lock.
mvn -B -q dependency:tree -DoutputType=text -DoutputFile=target/dependencies.lock
cmp dependencies.lock target/dependencies.lock
exec mvn -B -q compile exec:java
