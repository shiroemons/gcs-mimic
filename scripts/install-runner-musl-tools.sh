#!/usr/bin/env bash
# Installs musl-tools on an Ubuntu GitHub-hosted runner for the native musl build and test lanes.
# Run it as root (`sudo bash scripts/install-runner-musl-tools.sh`).
#
# Every apt source of the runner image lives under ubuntu.com (azure.archive.ubuntu.com on x86_64,
# ports.ubuntu.com on arm64). A single `apt-get` attempt failed every musl lane while those names
# did not resolve for hours on 2026-11-11, and a transient mirror error fails it as well. apt first
# retries with exponential backoff; if the runner's sources still fail, the same packages come from
# an official Ubuntu archive mirror outside ubuntu.com. apt checks the archive signature against the
# runner's Ubuntu keyring either way, so the fallback cannot substitute other packages.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

# Listed for both the ubuntu and ubuntu-ports archives on https://launchpad.net/ubuntu/+archivemirrors.
fallback=https://mirrors.ocf.berkeley.edu

install_musl_tools() {
  apt-get -o Acquire::Retries=8 "$@" update -qq &&
    apt-get -o Acquire::Retries=8 "$@" install -y -qq --no-install-recommends musl-tools
}

if install_musl_tools; then
  exit 0
fi

# shellcheck source=/dev/null
. /etc/os-release
case $(dpkg --print-architecture) in
  amd64 | i386) archive=ubuntu ;;
  *) archive=ubuntu-ports ;;
esac
echo "The runner's apt sources failed; installing musl-tools from $fallback/$archive" >&2
parts=$(mktemp -d)
trap 'rm -rf "$parts"' EXIT
cat >"$parts/fallback.sources" <<SOURCES
Types: deb
URIs: $fallback/$archive
Suites: $VERSION_CODENAME $VERSION_CODENAME-updates $VERSION_CODENAME-security
Components: main universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
SOURCES
install_musl_tools -o Dir::Etc::SourceList=/dev/null -o "Dir::Etc::SourceParts=$parts"
