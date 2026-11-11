# Sourced by the SDK runners that start their own gcs-mimic servers.
#
# Runners configure servers with flags only, so inherited server settings must not change
# their isolated scenarios. GCSM_ client and harness variables pass through unchanged.
# Matching the setting prefix also isolates settings added by later releases.

# Sets the array server_env to an `env` command prefix that removes each such variable exported
# now. Call it right before starting a server, so a variable exported since an earlier start is
# removed too. env runs the server in place, so `"${server_env[@]}" "$server" serve ... &`
# leaves the server's PID in $!.
set_server_env() {
  local name
  server_env=(env)
  for name in $(compgen -e); do
    case $name in
      GCS_MIMIC_* | PUBSUB_EMULATOR_HOST) server_env+=(-u "$name") ;;
    esac
  done
}
