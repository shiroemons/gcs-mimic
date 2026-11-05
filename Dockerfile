# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
FROM rust:1.99.1-slim-trixie@sha256:ea0999ff67355d958b8799b6751c879a6d770076d26047dca66f6e51e77930d3 AS build
ARG TARGETARCH
WORKDIR /build
# Cargo reads this as its job count. Two keeps `just check-containers`, which builds
# both architectures at once, within a small Docker host; CI raises it on its own runners.
ARG CARGO_BUILD_JOBS=2
RUN --mount=type=bind,source=scripts/install-musl-tools.sh,target=/app/scripts/install-musl-tools.sh \
    /app/scripts/install-musl-tools.sh
# The release workflow verifies the same pinned archive for the musl binary archives.
ADD --checksum=sha256:a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4 \
    https://musl.libc.org/releases/musl-1.2.5.tar.gz /build/target/release-notices/musl-1.2.5.tar.gz
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY xtask/Cargo.toml ./xtask/Cargo.toml
COPY xtask/src ./xtask/src
COPY src ./src
COPY compatibility/upstream/json-schemas.json ./compatibility/upstream/json-schemas.json
COPY LICENSE-MIT LICENSE-APACHE ./
COPY distribution/notices ./distribution/notices
# A static musl binary needs no C library, certificates or time zone data at run time, so the
# runtime starts from scratch. Fat LTO with opt-level "s" made the musl binary about 30% smaller
# and matched the default profile's throughput in a short local run, while "s" under the default
# thin LTO was slower; the release archives keep the default profile.
RUN case "$TARGETARCH" in \
        amd64) target=x86_64-unknown-linux-musl ;; \
        arm64) target=aarch64-unknown-linux-musl ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && triple=$(echo "$target" | tr - _) \
    && export "CC_$triple=musl-gcc" "CARGO_TARGET_$(echo "$triple" | tr a-z A-Z)_LINKER=musl-gcc" \
    && rustup target add "$target" \
    && cargo fetch --locked \
    && cargo build --locked --offline --release --target "$target" --bin gcs-mimic \
        --config 'profile.release.strip="symbols"' --config 'profile.release.lto="fat"' \
        --config 'profile.release.opt-level="s"' \
    && cp "target/$target/release/gcs-mimic" /gcs-mimic \
    && cp "/usr/share/doc/gcc-$(cc -dumpversion)-base/copyright" target/release-notices/GCC-COPYRIGHT \
    && cargo run --locked --offline --release --package xtask -- release notices "$target" /distribution-licenses \
    && install -d -o 10001 -g 10001 /runtime-data \
    && install -d -m 1777 /runtime-tmp

FROM scratch AS runtime
LABEL org.opencontainers.image.source="https://github.com/shiroemons/gcs-mimic"
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"
WORKDIR /app
# Imports spool to the temporary directory; scratch has none.
COPY --link --from=build /runtime-tmp/ /tmp/
COPY --link --from=build --chown=10001:10001 /runtime-data/ /var/lib/gcs-mimic/
COPY --link --from=build /gcs-mimic /usr/local/bin/gcs-mimic
COPY --link --from=build /distribution-licenses/ /usr/share/licenses/gcs-mimic/
# Preserve the numeric identity used by existing persistent volumes.
USER 10001:10001
EXPOSE 4443
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=2s --timeout=2s --start-period=2s --retries=15 CMD ["gcs-mimic", "healthcheck"]
ENTRYPOINT ["gcs-mimic"]
# Like fake-gcs-server's `-data /data`, a bucket/object tree mounted at /data seeds every start.
# scratch defines no PATH, and the exec-form HEALTHCHECK resolves `gcs-mimic` through it.
ENV PATH=/usr/local/bin GCS_MIMIC_LISTEN=0.0.0.0:4443 GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic GCS_MIMIC_DEFAULT_SEED_DIR=/data
CMD ["serve"]
