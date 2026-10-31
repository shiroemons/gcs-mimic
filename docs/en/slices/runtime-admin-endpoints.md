# Runtime endpoints and container readiness

<!-- markdownlint-configure-file {"MD013": false} -->

This local management slice extends DESIGN-067, DESIGN-068, DESIGN-057 and
UX-002. It does not implement reset/reseed, a memory backend, or a Google API
method. The feature ledger remains the implementation-status authority.

## Contract and sources

Checked on 2026-10-30 against fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`:

- [Reference management routes](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go) expose `GET /_internal/healthcheck` and `PUT /_internal/config`.
- [Reference configuration handler](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/config.go) accepts `externalUrl` and `publicHost`, ignores unknown fields, and leaves empty strings unchanged.
- Google's [canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) retain the actual signed host and resource path. Runtime configuration never rewrites signed input.
- Google's [resumable upload guide](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads) treats the returned session URI as a bearer capability. Changing advertised endpoints does not delete or retarget existing sessions.

The stricter validation, explicit administrative opt-in, authorization and
atomic snapshots below are local safety boundaries, not provider-exact wire
claims or exact reproduction of the reference's unrestricted handler.

## Readiness and Testcontainers

`GET` and `HEAD /_internal/healthcheck` use the same readiness test as
`/_internal/ready`: recovery and seeding must have completed, the catalog must
remain healthy, background tasks must be running, and shutdown must not have
revoked readiness. `/_internal/health` keeps its existing liveness behavior.
Probe bodies return 413 and unsupported methods return 405 with `Allow`.
The CLI accepts the old healthcheck path with an explicit `--url`.

The first ready JSON line includes `executable: "gcs-mimic"`, the package
`version`, `healthcheck_path: "/_internal/healthcheck"`, `admin_enabled`, and
`message: "server started at ENDPOINT"`. The existing listener addresses and
effective advertised API endpoint remain present. This preserves structured
startup consumers and supplies a recognizable log substring for generic
container log waits. Prefer an HTTP wait on the healthcheck path and choose
the image's listener scheme explicitly.

The executable remains `gcs-mimic`. Testcontainers code that replaces the
entrypoint with `fake-gcs-server` must change it to `gcs-mimic`; this slice adds
no executable alias. Library-specific Testcontainers wrappers, image-name
substitution, and final container verification are separate acceptance work.

## Opt-in endpoint changes

Management is off by default. Enable it only for an isolated local emulator:

```sh
gcs-mimic serve --scheme http --admin-enabled true
```

`GCS_MIMIC_ADMIN_ENABLED=true` and JSON `"admin_enabled": true` provide the
same setting with CLI > environment > JSON > default precedence. It does not
change authentication mode. A disabled route returns 404. In strict mode the
caller additionally needs `storage.buckets.update` in `seed_project`; anonymous
requests receive 401 and a registered reader without that permission gets 403.
Permissive mode permits the enabled operation without a bearer token in either
compatibility profile. This is a local management permission convention.

After discovering a dynamically mapped host port, update the advertised API
endpoint and optional public authority before clients initiate uploads:

```sh
curl --fail -X PUT -H 'Content-Type: application/json' \
  --data '{"externalUrl":"http://localhost:49152","publicHost":"localhost:49152"}' \
  http://localhost:49152/_internal/config
```

For strict mode add the registered administrative bearer token and configure
its project grants to match `seed_project`. Port 49152 is an example; use the
actual mapped port. The successful response is an empty HTTP 200.

- `externalUrl` is an absolute HTTP(S) base URL. An optional proxy path is preserved and trailing slashes are normalized. Credentials, query, fragment, malformed authorities and port zero are rejected. Host and forwarded headers never select this value.
- `publicHost` is a host authority, optionally with a port or bracketed IPv6 address. Scheme, credentials and path are rejected. Public-origin validation and the 32 distinct DNS-domain cap apply. The listener's primary scheme supplies the public origin's scheme.
- Both values are validated and replaced together. A failure changes neither. A newly conflicting API host that would select an XML virtual bucket is rejected. Missing, null or empty-string values leave the corresponding setting unchanged; unknown fields are ignored. Wrong known-field types, non-object bodies, duplicate known fields, trailing JSON and query parameters are rejected. Bodies are bounded to 64 KiB.
- Concurrent partial updates merge against the latest committed pair under a short write lock. No lock crosses an await. A request retains one immutable, reference-counted endpoint snapshot through routing, authorization, upload publication and response generation. Ordinary requests clone no authentication registry, catalog or whole configuration. Poisoned endpoint state returns 503 visibly.
- XML virtual-host object selection precedes management dispatch. An object named `_internal/config` remains an ordinary readable/writable object on a configured bucket host, whether management is enabled or disabled.
- New HTTP resource links, ACL links, resumable Locations and XML POST success URLs use the snapshot's API endpoint. XML path/virtual-host selection uses its public authority and domains. A new implicit public DNS domain replaces the previous implicit one; explicitly configured virtual-host domains remain available.
- Media/multipart upload and copy replay identity retains the stable startup endpoint rather than the mutable advertised endpoint. Retrying the same URI, token and body after a configuration update reuses the original generation; response links use the retrying request's current snapshot.
- Listener addresses, TLS identities, IAM/ACL/PAP state and stored objects are unchanged. Strict signatures still bind the raw original method, Host, path and headers. Constructing or changing an endpoint grants no access.
- Existing resumable session paths remain valid through any reachable endpoint; future initiations advertise the new endpoint. Completion responses use their own request's snapshot. A proxy prefix requires the proxy to strip that prefix before forwarding.
- Changes are process-local. Restart restores CLI/environment/file settings and preserves stored objects. This slice does not persist administrative URL changes.

## Acceptance and verification

| Test | Observable behavior |
| --- | --- |
| `local_admin_configuration_is_explicit_and_obeys_native_precedence` | Default off; JSON boolean, environment and CLI precedence; invalid boolean rejection |
| `legacy_healthcheck_and_startup_event_support_container_waits` | Native/legacy startup fields and old-path GET/HEAD/CLI probes; method/body rejection |
| `wire_probes_clock_and_unimplemented_routes` | The old-path alias follows the real readiness flag while liveness stays available |
| `config_route_is_disabled_by_default_and_changes_only_future_http_links` | Opt-in, API/ACL links, atomic invalid-pair rejection, body cap, no-op/unknown fields and nonpersistent settings across restart |
| `changed_mapped_endpoint_reaches_new_resumable_locations_and_keeps_old_sessions_usable` | New host-mapped Locations and completion links; earlier sessions still publish exact bytes |
| `config_update_during_upload_retains_the_requests_original_endpoint_snapshot` | An upload paused after 100 Continue renders its original URL while a later read uses the new URL |
| `public_host_updates_replace_implicit_routing_and_preserve_explicit_domains` | Native/legacy XML host routing changes without retaining an obsolete implicit suffix |
| `virtual_host_objects_named_like_management_routes_remain_readable_and_writable` | Native/legacy virtual-host GET/PUT of `_internal/config` keeps object semantics with management on or off |
| `endpoint_changes_preserve_media_multipart_and_copy_replay_identity` | URL changes preserve the same generation for media, multipart and copy token retries while updating response links |
| `strict_config_authorization_and_raw_signed_host_survive_public_host_updates` | Strict management 401/403, unchanged anonymous denial, valid signed read, wrong host/path rejection in both profiles |
| `config::runtime::tests` | Immutable snapshots, atomic rejection, concurrent partial updates, domain cap/explicit preservation and visible lock poisoning |

The native commands are `cargo check --locked --offline`,
`cargo test --locked --offline --lib config::runtime::tests`,
`cargo test --locked --offline --test admin_http --test healthcheck_defaults --test foundation -- --test-threads=4`,
and `cargo xtask lint`. Tests need permission to bind local TCP sockets.

Native verification on 2026-10-30: `cargo check --locked --offline` passed;
runtime unit tests passed 5/5, `admin_http` passed 9/9, `foundation` passed
28/28 and `healthcheck_defaults` passed 3/3. The production lint gate reported
0 violations. Existing replay regressions also passed 6/6 with
`cargo test --locked --offline --test object_http --test object_multipart_http --test object_copy_http replay -- --test-threads=4`.
Formatting of this slice's Rust files and `git diff --check`
passed; the full-checkout format gate remains the integration owner's check
while other slices are being edited concurrently.

## Remaining scope

Reset, reseed and bounded ephemeral storage require a separate shared Admin
capability created during explicit startup opt-in, with write/concurrency,
holds/retention, sync/import and crash-recovery cases. They remain unimplemented;
no stub returns success. A temporary disk-backed catalog is not a true RAM
backend and does not establish DESIGN-077 or a total-storage budget.

Automatic request-origin selection and trusted proxies remain deferred under
SPEC-RSM-LOCATION. One advertised origin does not solve disjoint host/container
DNS spaces. Offline URL helper commands and gRPC-rendered JSON links retain
their startup configuration. Final SDK, container and Testcontainers-wrapper
verification belongs to the integrated source, not this native slice.
