# shellcheck shell=bash
# Sourced by the container checks that assert which listeners a recipe publishes.
#
# HostConfig.PortBindings is only what was requested, and some Docker-compatible runtimes, such
# as Kiri, report it as null. NetworkSettings.Ports is what the runtime actually bound, so a check
# that reads it asserts the effective publication and works on every runtime. Ports that are
# exposed but not published appear there with no bindings and are left out.

# Prints the container ports published to the host as a sorted JSON array, such as
# ["4443/tcp"]. Argument: the container ID or name.
published_ports() {
  docker inspect "$1" | jq -c '[.[0].NetworkSettings.Ports // {} | to_entries[] | select(.value | length > 0) | .key] | sort'
}
