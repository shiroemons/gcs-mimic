#!/usr/bin/env bash
# Installs a pinned socat inside the digest-pinned Rust scorecard image (Debian trixie).
# run.sh and test-tls.sh run this script from the read-only /app mount, so the pin lives here only.
#
# A bare `apt-get install socat` would take whatever version the live mirror serves that day,
# and would break once the mirror drops the pinned version. Instead apt reads a fixed
# snapshot.debian.org archive, so socat and its dependencies (libssl3t64, libwrap0, libc6) are
# the same bytes on every run. The timestamp is the snapshot the Rust image itself was built
# from (see the comment in its /etc/apt/sources.list.d/debian.sources); advance it together
# with the rust_image digest. Output goes to stderr to keep the probe's stdout a pure JSONL stream.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

snapshot=20261104T212339Z
socat_version=1.8.0.3-1+deb13u1

install_from() {
  cat >/etc/apt/sources.list.d/debian.sources <<SOURCES
Types: deb
URIs: $1
Suites: trixie
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
SOURCES
  apt-get -o Acquire::Retries=3 update -qq >&2 &&
    apt-get -o Acquire::Retries=3 install -y -qq --no-install-recommends "socat=$socat_version" >&2
}

# snapshot.debian.org intermittently answers 503/504 under load. The live mirror keeps the exact
# socat version pin and apt's signature check, but its dependencies may be newer than the
# snapshot's; this development-only forwarder accepts that over failing the whole scorecard.
if ! install_from "http://snapshot.debian.org/archive/debian/$snapshot"; then
  echo "snapshot.debian.org failed; retrying the pinned socat version from deb.debian.org" >&2
  install_from "http://deb.debian.org/debian"
fi
