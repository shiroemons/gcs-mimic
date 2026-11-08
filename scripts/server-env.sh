# Sourced by the SDK runners that start their own gcs-mimic servers.
#
# The server reads every exported GCS_MIMIC_ and FAKE_GCS_ variable, and PUBSUB_EMULATOR_HOST,
# as a setting, and refuses to start on a GCS_MIMIC_ name it does not know. The runners
# configure each server with flags only, so a variable from the developer's shell, such as the
# strict local recipe's GCS_MIMIC_BUCKETS or GCS_MIMIC_TEST_SIGNER, or one a runner exports for
# its clients, such as GCS_MIMIC_ENDPOINT, would change or stop the server. Matching the
# prefixes instead of listing names also removes settings that later releases add.

# Sets the array server_env to an `env` command prefix that removes each such variable exported
# now. Call it right before starting a server, so a variable exported since an earlier start is
# removed too. env runs the server in place, so `"${server_env[@]}" "$server" serve ... &`
# leaves the server's PID in $!.
set_server_env() {
  local name
  server_env=(env)
  for name in $(compgen -e); do
    case $name in
      GCS_MIMIC_* | FAKE_GCS_* | PUBSUB_EMULATOR_HOST) server_env+=(-u "$name") ;;
    esac
  done
}
