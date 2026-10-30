# List the available development commands.
default:
    @just --list

# Run the pinned Rust and shell checks shared with CI.
check:
    bash scripts/test-pre-push.sh
    bash scripts/test-check.sh
    env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/check.sh

# Check upstream dependency freshness; requires network access.
check-dependencies:
    env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check

# Run the complete pinned Go, Ruby, Node.js, Python and PHP SDK suites against isolated servers.
check-sdk:
    env -u GCS_MIMIC_GO_TEST_RUN bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php

# Report Rails GCS compatibility cases independently; known failures return nonzero.
check-active-storage:
    bash scripts/sdk-active-storage.sh

# Build and smoke-test both Linux container architectures in parallel.
[parallel]
check-containers: (check-container "linux/amd64") (check-container "linux/arm64")

# Build and smoke-test one Linux container architecture.
check-container platform:
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/smoke-containers.sh {{ quote(platform) }} 2>&1 | \
        awk -v platform={{ quote(platform) }} '{ print "[" platform "] " $0; fflush() }'

# Run each check stage in order; the container stage waits for both architectures.
# Progress lines show the stage, overall percentage and the time left based on earlier runs.
pre-push: (_pre-push-progress "start") check (_pre-push-progress "check") check-sdk (_pre-push-progress "check-sdk") check-containers (_pre-push-progress "check-containers")

[private]
_pre-push-progress mark:
    @bash scripts/pre-push-progress.sh {{ quote(mark) }} {{ just_pid() }}

# Stage a built native binary with licenses and build information.
prepare-binary target binary output:
    env -u RUSTUP_TOOLCHAIN cargo xtask release prepare {{ quote(target) }} {{ quote(binary) }} {{ quote(output) }}
