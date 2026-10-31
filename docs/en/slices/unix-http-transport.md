# Private Unix HTTP transport

<!-- markdownlint-configure-file {"MD013": false} -->

`serve --unix-socket PATH --scheme http` uses HTTP/1.1 over a local Unix-domain socket, with no TCP or gRPC listener. This is a local transport extension for the [Go subprocess facade](go-subprocess-facade.md), not a Google endpoint or in-process/FFI embedding.

## Contract

- `PATH` must be new. Its immediate parent must be an existing private 0700 directory owned by the server user, not a symlink. The socket is 0600 before readiness. Existing files, directories, sockets and dangling symlinks fail without removal. Platform pathname-length failures produce an actionable startup diagnostic; there is no TCP fallback.
- Unix transport requires `scheme=http`. Supplied TCP `listen`, `listen_http`, `grpc_listen` or TLS settings are rejected. The default TCP address is unused, even when another process owns that port. Windows rejects the option explicitly.
- The ready record reports `transport: "unix"`, `unix_socket`, `listen: null` and an empty `listeners` array. Logs/ready JSON remain on their existing output streams; HTTP bytes use the socket exclusively.
- `api_base_url` and `public_base_url` retain their HTTP(S) meanings independently of the socket path. The default API origin is `http://localhost`; it is an advertised logical origin, not a TCP listener. The Go facade seals its DialContext to this instance's Unix socket, preserves request Host, and rejects other origins and redirects before dialing them. Explicit ExternalURL still takes precedence in `URL()`.
- Unix connections use the existing shared admission limit, bounded HTTP parser/body channels, request/inactivity/keepalive deadlines, panic isolation, dispatcher, authorization and catalog. No object implementation or permission bypass is added. Explicit local Pub/Sub delivery remains a separate opt-in network service.
- Socket filesystem work runs through one startup job and at most one cleanup job on the bounded blocking pool, with `catch_job` supervision. Normal shutdown closes admission and awaits inode-checked cleanup before returning. Canceled startup hands cleanup to the bounded worker; cancellation during bind is handled in the same worker if the waiting receiver has gone away, including runtime shutdown.
- Cleanup leaves a pathname whose inode/type no longer matches the owned socket. The unique private parent is part of the trust boundary; concurrent same-user replacement between stat and unlink is not claimed atomic. A process kill or host failure can leave a stale socket; a later start refuses it rather than guessing ownership. The Go parent owns and removes only its temporary socket directory after child exit.

## Acceptance

- `private_socket_permissions_and_explicit_close_remove_only_owned_entry`: 0600 permissions, Unix connection, awaited cleanup, preservation of an unrelated replacement.
- `existing_files_sockets_symlinks_and_nonprivate_parents_fail_without_unlink`: every existing entry is rejected; a dangling symlink's target is not created. This negative test detected macOS bind following a dangling symlink, which the pre-bind lstat check now prevents.
- `canceled_bind_reclaims_its_socket_even_during_runtime_shutdown`: queued bind cancellation followed by immediate runtime shutdown leaves no socket pathname.
- `unix_socket_configuration_has_no_implicit_tcp_or_tls_fallback`: supported configuration and explicit native/environment TCP/TLS conflicts.
- `unix_startup_does_not_bind_the_configured_default_tcp_address`: startup succeeds on Unix while the configured default TCP address is already occupied, with no HTTP/gRPC TCP listener.
- `unix_wire_uses_shared_crud_conditions_parallel_clients_and_graceful_cleanup`: real subprocess ready state, bucket/object CRUD, advertised links, failed generation condition, eight parallel reads with four admission slots, successful shutdown and rejected subsequent connections.
- `unix_cli_rejects_explicit_tcp_tls_and_foreign_entries_before_ready`: invalid combinations and foreign paths never become ready or remove the prior file.

Run `cargo check --locked --offline --tests` before the targeted tests:

```sh
cargo test --locked --offline --lib server::unix::tests
cargo test --locked --offline --lib config::unix_tests
cargo test --locked --offline --bin gcs-mimic unix_startup
cargo test --locked --offline --test unix_transport
cargo xtask lint
```

The native wire/configuration tests and Go facade tests are separate evidence. Final SDK/container execution, other Unix platforms, Windows runtime operation and OS-level denial of every Internet/TCP syscall have not been verified by this slice. No speed or memory benchmark is claimed.
