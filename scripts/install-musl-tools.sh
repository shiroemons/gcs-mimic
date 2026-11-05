#!/usr/bin/env bash
# Installs pinned musl-tools inside the digest-pinned Rust build image (Debian trixie). The
# Dockerfile bind-mounts this script read-only at /app, so the pin lives here only.
#
# musl-gcc compiles the C dependencies (SQLite, ring) and links the static container binary.
# A bare `apt-get install musl-tools` would take whatever version the live mirror serves that
# day. Instead apt reads a fixed snapshot.debian.org archive: the timestamp is the snapshot the
# Rust image itself was built from (see the comment in its
# /etc/apt/sources.list.d/debian.sources); advance it together with the rust_image digest.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

snapshot=20261027T000000Z
musl_version=1.2.5-3.1~deb13u1

cat >/etc/apt/sources.list.d/debian.sources <<SOURCES
Types: deb
URIs: http://snapshot.debian.org/archive/debian/$snapshot
Suites: trixie
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
SOURCES

# snapshot.debian.org intermittently answers 503/504 under load, and this script now gates the
# release image. apt backs off exponentially between retries, so eight attempts ride out a
# couple of minutes of overload instead of the few seconds three attempts cover.
apt-get -o Acquire::Retries=8 update -qq
apt-get -o Acquire::Retries=8 install -y -qq --no-install-recommends \
  "musl-tools=$musl_version" "musl-dev=$musl_version" "musl=$musl_version"
rm -rf /var/lib/apt/lists/*
