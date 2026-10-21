# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
FROM rust:1.99.1-slim-trixie@sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY xtask/Cargo.toml ./xtask/Cargo.toml
COPY xtask/src ./xtask/src
COPY src ./src
RUN cargo build --locked --release --bin gcs-mimic --jobs 2

FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad AS runtime
LABEL org.opencontainers.image.source="https://github.com/shiroemons/gcs-mimic"
WORKDIR /app
RUN groupadd --gid 10001 gcs-mimic && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin gcs-mimic
COPY --from=build --chown=10001:10001 /build/target/release/gcs-mimic /usr/local/bin/gcs-mimic
USER 10001:10001
EXPOSE 4443
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=2s --timeout=2s --start-period=2s --retries=15 CMD ["gcs-mimic", "healthcheck", "--url", "http://127.0.0.1:4443/_internal/ready"]
ENTRYPOINT ["gcs-mimic"]
CMD ["serve", "--listen", "0.0.0.0:4443"]
