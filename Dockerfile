# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
FROM rust:1.99.1-slim-trixie@sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6 AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY xtask/Cargo.toml ./xtask/Cargo.toml
COPY xtask/src ./xtask/src
COPY src ./src
COPY LICENSE-MIT LICENSE-APACHE ./
COPY distribution/notices ./distribution/notices
RUN cargo fetch --locked \
    && cargo build --locked --offline --release --bin gcs-mimic --jobs 2 --config 'profile.release.strip="symbols"' \
    && mkdir -p target/release-notices \
    && cp "/usr/share/doc/gcc-$(cc -dumpversion)-base/copyright" target/release-notices/GCC-COPYRIGHT \
    && cargo run --locked --offline --release --package xtask -- release notices /distribution-licenses \
    && install -d -o 10001 -g 10001 /runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf AS runtime
LABEL org.opencontainers.image.source="https://github.com/shiroemons/gcs-mimic"
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"
WORKDIR /app
COPY --link --from=build --chown=10001:10001 /runtime-data/ /var/lib/gcs-mimic/
COPY --link --from=build /build/target/release/gcs-mimic /usr/local/bin/gcs-mimic
COPY --link --from=build /distribution-licenses/ /usr/share/licenses/gcs-mimic/
# Preserve the numeric identity used by existing persistent volumes.
USER 10001:10001
EXPOSE 4443
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=2s --timeout=2s --start-period=2s --retries=15 CMD ["gcs-mimic", "healthcheck", "--url", "http://127.0.0.1:4443/_internal/ready"]
ENTRYPOINT ["gcs-mimic"]
CMD ["serve", "--listen", "0.0.0.0:4443", "--data-dir", "/var/lib/gcs-mimic"]
