#!/usr/bin/env bash
# Build and run the Java SDK scorecard probe.
#   run.sh build          verify dependencies.lock, compile and record the runtime classpath
#   run.sh run [args...]  run the compiled probe (stdout is JSONL; --transport-check is accepted)
#   run.sh [args...]      build, then run
set -euo pipefail
cd "$(dirname "$0")"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY GRPC_PROXY_EXP \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT
export GCE_METADATA_DISABLED=true
# google-auth-library-java skips its Compute Engine probe only for NO_GCE_CHECK, and an empty
# gcloud config directory keeps user ADC files and gcloud projects out of default lookups.
export NO_GCE_CHECK=true
export CLOUDSDK_CONFIG="$PWD/target/empty-gcloud-config"
mvn_flags=(--batch-mode --no-transfer-progress --quiet)

build() {
  mvn "${mvn_flags[@]}" dependency:tree -DoutputType=text -DoutputFile=target/dependencies.lock
  cmp dependencies.lock target/dependencies.lock
  mvn "${mvn_flags[@]}" compile dependency:build-classpath -Dmdep.outputFile=target/classpath.txt
}

run() {
  [[ -f target/classpath.txt && -d target/classes ]] || { echo 'run.sh: run "run.sh build" first' >&2; exit 2; }
  mkdir -p "$CLOUDSDK_CONFIG"
  # Running the JVM directly keeps Maven output out of the JSONL stream.
  exec "${JAVA_HOME:+$JAVA_HOME/bin/}java" -classpath "target/classes:$(cat target/classpath.txt)" \
    local.gcsmimic.scorecard.Probe "$@"
}

case "${1:-}" in
  build) build ;;
  run) shift; run "$@" ;;
  *) build; run "$@" ;;
esac
