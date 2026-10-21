# Integrated object listing and metadata verification

Verified on **2026-10-22** (local date), macOS arm64, Rust 1.99.1 / edition 2024.
Runtime source: `a31bc3d`, including the catalog deadline test synchronization fix
`e5279a7`; subsequent changes only update documentation, evidence and ledger links.
Go 1.27.3, Storage v1.69.0 and API v0.307.0 use the committed module graph.
No real Cloud Storage or metadata-server request was made.

## Integrated behavior

The [listing](../slices/json-object-list.md) and
[metadata](../slices/json-object-metadata.md) contracts retain their scoped PARTIAL
status for JSON-OBJECTS-008/010/015. Collection GET lists; collection POST retains
its unimplemented 404; object PATCH/PUT mutate metadata atomically. Both list and
GET select the v4 metadata columns through the shared resource renderer.

Changed ledger rows are JSON-OBJECTS-008/010/015, BEHAVIOR-COMMON-006/009/011
and BEHAVIOR-OBJECT-001/002/006; all are PARTIAL. JSON-OBJECTS-010/015 and
BEHAVIOR-COMMON-006 advance from NOT_STARTED; the others retain PARTIAL with
additional behavior or evidence.

`object_list_resources_defaults_empty_and_restart` now patches contentType,
cacheControl and custom metadata under `ifMetagenerationMatch=1`, asserts
metageneration 2 and compares each full listed resource with GET, before and
after restart. The bounded SQL prefix-seek fixture applies the v4 migration
without relaxing its 10,000-instruction limit.

## Final verification

| Command or workflow | Result | Evidence |
| --- | --- | --- |
| `just pre-push` | PASS, exit 0; one final-source run | [Complete log](object-list-metadata-checks.txt) |
| Rust checks within `pre-push` | fmt, clippy, 169 tests; zero failed, ignored or filtered; ledger check and shell/release checks pass | Same complete log |
| Full Go suite within `pre-push` | vet and every test pass, including listing and metadata Update | Same complete log |
| Container checks within `pre-push` | amd64 and arm64 build/smoke pass; probes, host/peer CRUD, persistence, reset, limits and SIGINT/SIGTERM | Same complete log |
| Every `sh` block in `docs/en/getting-started.md`, in order under Bash `-euxo pipefail` | PASS, exit 0; Docker build through cleanup and native release build/start/shutdown | [Guide transcript](object-list-metadata-guide.txt) |
| `cargo xtask dependencies check` | PASS, exit 0; every input CURRENT | [Freshness log](object-list-metadata-dependencies.txt) |

The guide exercises prefix/delimiter grouping, one-entry pages and continuation;
conditional PATCH (metageneration 2), stale PATCH (412), replacement PUT
(metageneration 3), exact list/resource equality, unchanged bytes, stop/start and
down/up persistence, object/bucket deletion, deliberate volume reset, the full Go
suite against Compose and native binary startup. Its temporary catalogs, Compose
resources and exact guide image tag were removed by the documented commands.
Raw logs replace only local path prefixes with placeholders and declare that
redaction on their first line. The earlier slice logs remain historical evidence;
this final integrated set supersedes their incomplete integration checks.

Markdown lint uses markdownlint-cli2 v0.24.0: MD013 is disabled for existing long
API tables/URLs/commands; MD024 uses `siblings_only=true` for generated category
headings. Generated English/Japanese matrices come from the ledger; the generator
does not rewrite the parameter/schema-field inventories or inventory summary.

## Limits and next slice

Native amd64, remote CI, Ruby/Rails, real-service differential behavior, power-loss
durability, every fsync crash boundary and performance/resource measurements remain
unverified. IAM/ACL, historical/soft-deleted listing, glob/context filters, partial
response masks, retry replay and streaming gzip transcoding remain unsupported.
Both container architectures were exercised locally; amd64 uses emulation.

The next slice is JSON multipart upload, completing the next P1 transfer subset
needed by Go Writer before P2 public/private access. README's manually maintained
object-list/pagination/PATCH/PUT cells, current object API limitation text and API
v0.306.0 SDK pin are stale; they were deliberately not hand-edited in this task.
