# Breaking changes before the 1.0.0 contract freeze

<!-- markdownlint-configure-file {"MD013": false} -->

DESIGN-103 collects the `0.x` breaking changes that the [public contract review](../public-contract-review.md) decided on 2026-11-12, so that `1.0.0` freezes names, defaults and checks that need no alias. They ship together in `0.20.0`, so a configuration changes once. The [feature ledger](../../../compatibility/features.json) owns the status; the [verification record](../evidence/contract-cleanup.md) lists what ran.

## Problem

A name kept through `1.x` as an alias needs the accepted-with-a-warning path of the [release policy](../release-policy.md), which no code has yet. In `0.x` a withdrawn input can instead stop startup with advice, as the client variables renamed in `0.18.0` do. Each item below was cheap to change now and would cost an alias, or a major release, after the freeze.

## Decisions

| Change | Review candidate |
| --- | --- |
| `buckets_mode` replaces `buckets_file_mode` and defaults to `reconcile` for a file too; the ready field `buckets_file` becomes `buckets` | 3, 8 |
| `connection_timeout_ms` is removed | 10 |
| Server options written with underscores, such as `--max_connections`, are refused | 13 |
| `backend=filesystem` is accepted only as the fake-gcs-server flag | 14 |
| `gcs-mimic sync` and the server refuse each other across releases | 6 |
| `sync approve` refuses `--revision` | 9 |
| The Go package refuses a binary of another release | 18 |
| `compat_profile=legacy-emulator` becomes `fake-gcs-server` | 12 |

None has an alias. Every withdrawn name or value stops startup from the command line, the environment and the JSON file with one diagnostic that names its replacement as that source spells it: `--buckets-mode`, `GCS_MIMIC_BUCKETS_MODE` or `buckets_mode`.

## Behavior

### Withdrawn settings and values

- A withdrawn setting name is a `WithdrawnSetting` in `src/config/rejection.rs`, which every source reaches through `Input::setting`. A rename names its successor; `connection_timeout_ms` names the three settings it used to set.
- A withdrawn value is listed in the `withdrawn` clause of its `string_enum!`, so parsing it fails with its own fix: `compat_profile=legacy-emulator` names `fake-gcs-server`, and `backend=filesystem` names `disk`.
- The fake-gcs-server flag `-backend filesystem` still translates to `backend=disk`; only the native setting refuses the value.
- An underscore spelling of a server option fails as one `cli` diagnostic keyed as supplied. Its value is still consumed, so no second diagnostic follows. Environment and JSON names keep their underscores, and helper commands are unaffected.

### Bucket initialization

`buckets_mode=reconcile` is the default for both document settings: each start writes the members an entry writes whose stored value differs, so an API change to such a member is undone and an edit to the document applies. `create` keeps existing buckets as they are and warns about an edited entry. The public access prevention check is the same in both modes, so a written public `iamPolicy` against stored prevention stops startup in either. The [contract](bucket-initialization.md) has the details.

### Synchronization controls

`gcs-mimic sync` sends `x-gcs-mimic-version` with its version, and every answer of a `/_internal/sync` route carries the server's. The server checks the header first in its control path, after the browser and host guards and before authentication and any catalog wait, and answers 400 `versionMismatch` naming both versions. The command prints the answer as always, then refuses one from another release or without a version before judging the status. See the [CLI contract](sync-cli.md).

### Go package

`go/fakestorage` records its release in `moduleVersion`, which `scripts/check-release-recipes.sh` keeps equal to `Cargo.toml`. It reads `version` from the ready event and accepts the same `0.x` minor release, or from `1.0.0` the same major at or above its minor. When a binary exits before ready, it asks `--version` once and adds the mismatch to the exit error if that is the cause. Another release fails with `*UnsupportedBinaryError`; see the [package README](../../../go/README.md).

## Acceptance cases

| Case | Required behavior |
| --- | --- |
| DESIGN-103-BUCKETS-MODE | Unset `buckets_mode` reconciles a file and an inline document alike, undoing an API change to a written member and applying an edit; an unchanged restart writes nothing; `create` keeps buckets and warns; both modes refuse a public policy against stored prevention; the ready field is `buckets`. |
| DESIGN-103-WITHDRAWN-NAMES | `buckets_file_mode` and `connection_timeout_ms` stop startup from each source with one diagnostic that names the replacement in that source's spelling. |
| DESIGN-103-WITHDRAWN-VALUES | `compat_profile=legacy-emulator` and `backend=filesystem` stop startup from each source naming the new value; the `-backend filesystem` flag still starts with `disk`; `fake-gcs-server` behaves as `legacy-emulator` did. |
| DESIGN-103-UNDERSCORE | `--max_connections 10` and `-max_connections=10` exit 1 before a catalog with one diagnostic naming `--max-connections`; helper commands are unchanged. |
| DESIGN-103-SYNC-VERSION | A server refuses another version and a missing one with 400 `versionMismatch` before authorization or any wait; the command refuses an answer from another release or without a version and names both; matching releases work as before. |
| DESIGN-103-SYNC-APPROVE | `sync approve --revision` exits 1 without a request and names `--deletion-set`. |
| DESIGN-103-GO-VERSION | The Go package starts a binary of its minor release and refuses another one, or a ready event without a version, with `*UnsupportedBinaryError`; an early exit of another release reports the version, and of the same release keeps its own error. |

## Migration

| If you | Then |
| --- | --- |
| Set `buckets_file_mode` | Rename it `buckets_mode`. |
| Rely on a bucket file keeping API changes to the fields its entries write | Set `buckets_mode=create`. |
| Read the ready field `buckets_file` | Read `buckets`. |
| Set `connection_timeout_ms` | Set `header_read_timeout_ms`, `body_inactivity_timeout_ms` and `request_timeout_ms`. |
| Write server options with underscores | Write them with hyphens. |
| Set `backend=filesystem` in the environment or JSON | Set `disk`. |
| Set `compat_profile=legacy-emulator` | Set `fake-gcs-server`. |
| Use `sync approve --revision` | Use `--deletion-set`. |
| Run `gcs-mimic sync` or the Go package against another release | Use the server's release, or build the binary from the package's checkout. |

## Limits

- A `0.20.0` command cannot recognize a server before `0.20.0` until that server has handled the request. It says so, so that a mutation is inspected before it is retried.
- The Go package's range check needs `moduleVersion` to move with each release; the recipe check fails when it does not.
- The withdrawn names and values are refused for good. After `1.0.0`, the [release policy](../release-policy.md) deprecates with a warning first.
