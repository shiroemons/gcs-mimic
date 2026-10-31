#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT
export GCE_METADATA_DISABLED=true
mvn --batch-mode --no-transfer-progress dependency:tree -DoutputType=text -DoutputFile=target/dependencies.lock
cmp dependencies.lock target/dependencies.lock
# Testcontainers' process-lifetime daemons need ordinary JVM shutdown, outside Maven's thread cleanup.
exec mvn --batch-mode --no-transfer-progress compile exec:exec \
  "-Dexec.executable=${JAVA_HOME:+$JAVA_HOME/bin/}java" \
  "-Dexec.args=-classpath %classpath local.gcsmimic.Scenario ${*:-}"
