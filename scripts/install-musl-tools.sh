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

snapshot=20261104T212339Z
musl_version=1.2.5-3.1~deb13u1

install_from() {
  cat >/etc/apt/sources.list.d/debian.sources <<SOURCES
Types: deb
URIs: $1
Suites: trixie
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
SOURCES
  apt-get -o Acquire::Retries=8 update -qq &&
    apt-get -o Acquire::Retries=8 install -y -qq --no-install-recommends \
      "musl-tools=$musl_version" "musl-dev=$musl_version" "musl=$musl_version"
}

# snapshot.debian.org intermittently answers 503/504 under load, and this script now gates the
# release image. apt backs off exponentially between retries, so eight attempts ride out a
# couple of minutes of overload instead of the few seconds three attempts cover. An outage can
# outlast them, so the live mirror is the fallback: the exact version pin and apt's signature
# check still select the same package, and a mirror that has dropped it fails the build.
if ! install_from "http://snapshot.debian.org/archive/debian/$snapshot"; then
  echo "snapshot.debian.org failed; retrying the pinned musl version from deb.debian.org" >&2
  install_from "http://deb.debian.org/debian"
fi
rm -rf /var/lib/apt/lists/*
