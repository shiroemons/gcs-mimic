# List the available development commands.
default:
    @just --list

# Run the pinned Rust and shell checks shared with CI.
check:
    bash scripts/test-pre-push.sh
    bash scripts/test-check.sh
    bash scripts/test-trim-target.sh
    bash scripts/trim-target.sh
    env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/check.sh

# Run the Rust tests repeatedly with parallel binaries on a busy CPU to surface timing-dependent tests.
stress-tests:
    env -u RUSTUP_TOOLCHAIN bash scripts/stress-tests.sh

# Drop stale incremental state and keep target/ under GCSM_TARGET_MAX_MB (default 30720).
trim-target:
    bash scripts/trim-target.sh

# Check upstream dependency freshness; requires network access.
check-dependencies:
    env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check

# Lint every tracked Markdown file with the current markdownlint-cli2; requires npm registry access.
check-markdown:
    bash scripts/check-markdown.sh

# Run the complete pinned Go, Ruby, Node.js, Python, PHP, Java, .NET and Rust SDK suites against isolated servers.
check-sdk:
    env -u GCS_MIMIC_GO_TEST_RUN bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php --with-java --with-dotnet --with-rust

# Run the pinned Java, .NET and Rust clients plus the Go facade/export adapter.
check-sdk-clients:
    env -u GCS_MIMIC_GO_TEST_RUN bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-clients

# Exercise strict local JWT authentication and final-origin Rails signing.
check-active-storage-strict:
    bash sdk-tests/ruby/active_storage/run-strict.sh

# Use an already built final-source image; no duplicate image build is performed.
check-client-containers image:
    GCS_MIMIC_IMAGE={{ quote(image) }} bash sdk-tests/java/run.sh --container
    GCS_MIMIC_IMAGE={{ quote(image) }} bash sdk-tests/dotnet/run.sh --container
    cd sdk-tests/testcontainers/node && npm ci --ignore-scripts --no-audit --no-fund && GCS_MIMIC_IMAGE={{ quote(image) }} node scenario.mjs

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
