# Migration defaults

<!-- markdownlint-configure-file {"MD013": false} -->

## Contract

Scope: DESIGN-067, DESIGN-058 and JSON-BUCKETS-006. Existing HTTP deployments
need an image probe that follows their listener configuration, and migration
inspection commands need the same omitted-project default as bucket creation.
No new configuration keys or startup phases are introduced.

Without `--url`, healthcheck loads the ordinary environment and JSON configuration
with server precedence, selects `/_internal/ready`, maps wildcard addresses to
loopback and prefers the HTTP listener in `both` mode. HTTPS trusts the embedded
development CA unless `--ca-cert` supplies another CA. Explicit `--url` bypasses
server configuration. CLI-only server overrides cannot be discovered by another
process; use environment/configuration inputs or an explicit probe URL.
For a listener configured with port zero, use an explicit URL containing the
allocated port or update the shared probe configuration after startup.

The image retains its existing wildcard listener and persistent directory as
`GCS_MIMIC_LISTEN=0.0.0.0:4443` and `GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`
instead of CMD arguments, so environment overrides reach both server and probe.
These image environment defaults take precedence over JSON file values.

The omitted-project decision is settled here: `legacy-emulator` uses
`seed_project` for bucket listing as it does for creation; native still requires
project and explains `?project=local-project`. Explicit invalid projects remain
invalid, and project filtering and authorization remain in force.

## Acceptance cases

| Case | Required behavior |
| --- | --- |
| healthcheck-default-url | Environment overrides JSON configuration; wildcard IPv4/IPv6 map to loopback; HTTPS trusts the development CA; both selects HTTP; explicit URL bypasses configuration |
| healthcheck-container-defaults | Rust process tests cover HTTP, HTTPS, both and changed ports; the final image becomes healthy in default HTTPS and HTTP without probe overrides |
| healthcheck-failure-diagnostic | Configuration and connection failures identify the derived URL and a matching repair example; HTTPS includes an issuing CA example |
| buckets-list-without-project-legacy | Omitted project lists only seed_project, with a Go migration scenario and Rust wire test |
| buckets-list-without-project-native | Native returns 400 and an example query; explicit invalid project is rejected |

## Guide mapping

The migration correspondence table adds the Container readiness, Image listener
and data defaults, and Bucket listing without project rows. Container usage and
its Japanese companion use the image probe without an HTTP override and explain
that image environment defaults override JSON, with environment override examples.

## Sources and boundaries

The [official buckets.list contract](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list)
requires project; the native requirement remains unchanged. The packaged JSON
Discovery baseline is revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
The optional project default is an explicit local compatibility profile behavior,
not a Google contract claim. Existing SPEC-012/017 error-parity and broader
bucket gaps remain open. The source comparison baseline remains fake-gcs-server
v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`.

## Verification

Verified on 2026-10-25 with Rust 1.99.1 and the committed lockfiles:

- Targeted Rust wire/process tests passed: profile-specific omitted project,
  explicit project filtering/rejection, HTTP, HTTPS, both, changed ports,
  environment-over-file precedence, wildcard mapping, explicit URL and diagnostics.
- `just check-sdk` passed with Go storage v1.70.0 and Ruby 4.0.9 /
  google-cloud-storage 1.62.1. The added Go migration HTTP case passed separately
  with `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator
  GCS_MIMIC_GO_TEST_RUN=TestMigrationAppScenarioBucketList bash scripts/sdk-go.sh`.
  The generated Go buckets.list call serializes an empty project explicitly;
  the existing migration HTTP helper is used to exercise an absent query instead.
- The final `docker build -t gcs-mimic:migrate-defaults .` produced local Linux ARM64
  manifest `sha256:4213f22f9727e80c1e8ead05dba250e410cc2e864f1b197ca55403329df20639`.
  Two read-only containers, default HTTPS and `GCS_MIMIC_SCHEME=http`, both became
  healthy with the unchanged image probe `gcs-mimic healthcheck`. The HTTP
  container also passed the guide's bucket creation, upload and byte retrieval.
- `cargo xtask dependencies check` passed: the pinned compiler, direct libraries,
  SDKs and container inputs were current. No dependency or setting was added.
- Final `just check` passed: formatting, Clippy with warnings denied, all Rust
  tests, compatibility validation and the existing shell/release checks.
- `cargo xtask compatibility render` and `cargo xtask compatibility check`
  passed for 591 feature rows. Both English and Japanese matrices were regenerated
  without a diff because the parent statuses and fidelity remain unchanged.
- Markdown lint passed for all eight edited Markdown files.

No performance measurement or real-cloud test was run. Other container
architectures and real browser enforcement were not
tested in this slice. Existing broad JSON bucket and readiness limitations remain
PARTIAL; this slice does not establish full fake-gcs-server replacement.
