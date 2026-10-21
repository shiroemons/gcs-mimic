# List the available development commands.
default:
    @just --list

# Run the pinned Rust and shell checks shared with CI.
check:
    bash scripts/test-check.sh
    env -u RUSTUP_TOOLCHAIN bash scripts/check.sh

# Check upstream dependency freshness; requires network access.
check-dependencies:
    env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check

# Run the complete pinned Go SDK suite against an isolated server.
check-sdk:
    env -u GCS_MIMIC_GO_TEST_RUN bash scripts/sdk-go.sh

# Build and smoke-test both Linux container architectures.
check-containers:
    bash scripts/smoke-containers.sh

# Stop on the first failure; run before pushing the final source.
pre-push: check check-sdk check-containers
