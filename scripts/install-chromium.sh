#!/usr/bin/env bash
# Installs a pinned Chromium inside the digest-pinned Node browser image (Debian trixie).
# scripts/strict-recipe-browser.sh copies this script to /app/scripts/ and runs it there.
#
# A bare `apt-get install chromium` would take whatever build the live mirror serves that day, so
# the same commit could test a different browser from one day to the next. Instead apt reads a
# fixed snapshot.debian.org archive, so Chromium and its dependencies are the same bytes on every
# run. The timestamp matches the snapshot the Node and Rust images were built from (see the comments
# in their /etc/apt/sources.list.d/debian.sources); the daily image refresh advances it together
# with the Rust image digest. The version is the one the trixie suite carries, which daily detection
# compares against it; a security-only build in trixie-security is not offered by this archive.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

snapshot=20261104T212339Z
chromium_version=150.0.7871.181-1~deb13u1

install_from() {
  cat >/etc/apt/sources.list.d/debian.sources <<SOURCES
Types: deb
URIs: $1
Suites: trixie
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
SOURCES
  apt-get -o Acquire::Retries=8 update -qq >&2 &&
    apt-get -o Acquire::Retries=8 install -y -qq --no-install-recommends "chromium=$chromium_version" >&2
}

# snapshot.debian.org intermittently answers 503/504 under load. The live mirror keeps the exact
# version pin and apt's signature check, but its dependencies may be newer than the snapshot's;
# this development-only browser accepts that over failing the whole check.
if ! install_from "http://snapshot.debian.org/archive/debian/$snapshot"; then
  echo "snapshot.debian.org failed; retrying the pinned Chromium version from deb.debian.org" >&2
  install_from "http://deb.debian.org/debian"
fi
