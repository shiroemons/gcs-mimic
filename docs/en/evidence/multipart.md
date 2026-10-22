# Integrated JSON multipart upload verification

Verified on **2026-10-22** (local date), macOS arm64, Rust 1.99.1 / edition 2024.
Final code/test source: `9800bd6c84e3c040e8abbc0acf5ca2c191154823`.
The last runtime-code change is `0aec65af63082b700289617a65b131c284be268f`,
which is the affected ledger rows' `implemented_in`; the later commit strengthens
negative and deadline tests. Subsequent changes update docs, evidence and the ledger.
Go 1.27.3, Storage v1.69.0 and API v0.307.0 use the committed module graph.
No real Cloud Storage or metadata-server request was made.

## Integrated behavior

The [multipart contract](../slices/json-object-multipart.md) defines single-request
JSON metadata/media upload, with an optional final JSON checksum part. It reuses
bounded admission/staging, incremental MD5/CRC32C, atomic generation conditions,
opaque blobs, catalog publication/recovery and transfer/catalog deadlines.
Malformed or truncated containers, failed checksums and failed conditions preserve
the previous complete resource and bytes; concurrent create-if-absent has one winner.
Successful fixed/custom metadata, hashes and generation survive restart.

Metadata types and the 8192-byte custom UTF-8 key/value limit are checked before
media input/staging. Query name overrides only string/null metadata name; the path
bucket overrides string metadata bucket. Nonempty metadata contentType wins over
the media header then `application/octet-stream`; null/empty contentType falls back.
Each JSON part is capped at 65536 bytes. The contract records exact MIME cap
accounting, error reasons and explicitly unsupported semantics.

Media/multipart uploads accept one nonempty invocation token only with
`ifGenerationMatch`, including zero. Without it they return 501 before body input;
empty/duplicate tokens return 400. A committed conditioned retry returns 412 without
another generation. This is generation protection, without response replay or
token-keyed deduplication. The Go `ChunkSize=0` Writer passes empty/binary payload,
fixed/custom metadata, explicit hash, DoesNotExist and conditioned-overwrite cases,
followed by Attrs and JSON NewReader. Resumable Writer still fails explicitly.

## Ledger decisions

| Row | Final status | Achieved fidelity |
| --- | --- | --- |
| BEHAVIOR-OBJECT-009, Multipart JSON upload | PARTIAL | LOCAL_SIMULATION |
| JSON-OBJECTS-007, objects.insert | PARTIAL | LOCAL_SIMULATION |
| BEHAVIOR-OBJECT-001, system/custom metadata | PARTIAL | LOCAL_SIMULATION |
| BEHAVIOR-OBJECT-005, checksums | PARTIAL | LOCAL_SIMULATION |
| BEHAVIOR-COMMON-011, generation/metageneration conditions | PARTIAL | LOCAL_SIMULATION |
| BEHAVIOR-OBJECT-008, simple media upload | PARTIAL | LOCAL_SIMULATION |
| DESIGN-082, official Go SDK | PARTIAL | PROJECT |

The multipart row advances from NOT_STARTED; the others retain PARTIAL with new
acceptance/evidence. All scoped multipart acceptance cases pass, with exact Rust
and Go test names. The policy requires no deviations and achieved fidelity equal
to target before IMPLEMENTED can be claimed. Explicit PENDING multipart cases retain
direct-service edge evidence, bounded token replay and ACL/IAM permissions.
SPEC-013 remains OPEN with conditioned upload-token acceptance; new SPEC-016 remains
OPEN for MIME limits/errors, contentType precedence and checksum-container choices.
No whole-method or exact Google compatibility claim follows from local success.

## Final verification

| Command or workflow | Result | Evidence |
| --- | --- | --- |
| `just pre-push` | PASS, exit 0; one final-source run | [Complete log](multipart-checks.txt) |
| Rust checks within `pre-push` | fmt, clippy, 194 tests; zero failed, ignored or filtered; ledger and shell/release checks pass | Same complete log |
| Full Go suite within `pre-push` | vet and all six top-level tests pass without skips, including TestObjectMultipartWriter | Same complete log |
| Container checks within `pre-push` | amd64/arm64 build and smoke pass: probes, host/peer media CRUD, persistence/reset, limits and SIGINT/SIGTERM | Same complete log |
| Every `sh` block in `docs/en/getting-started.md`, in order under Bash `-euxo pipefail` | PASS, exit 0; Docker build through cleanup and native release startup/shutdown | [Guide transcript](multipart-guide.txt) |
| `cargo +1.99.1 xtask compatibility validate`, `render`, `check`; Markdown lint and local-path checks | PASS; final ledger and both generated matrices agree | [Ledger and documentation checks](multipart-ledger.txt) |
| Staged whitespace check | Raw Compose logs report trailing spaces (exit 2); check excluding raw evidence TXT files passes | Same ledger/documentation log |
| Slice-start `just check-dependencies` (retained dependency-update evidence) | Recorded PASS, every input CURRENT; not rerun during final integration | [Freshness log](multipart-dependencies.txt) |

The guide's new multipart body includes custom metadata and explicit contentType.
It compares upload/GET metadata and exact downloaded bytes before deleting the
object. Existing media, list/metadata, peer-container, persistence, reset and Go
SDK steps also pass. Its temporary catalogs, Compose resources and exact image tag
are removed by the documented commands. Container smoke tests retain their media
scope; multipart wire tests run natively and the full Go suite runs natively and
against Compose in the guide.

Tokio 1.54.1 and Rust builder index
`sha256:f938561c139ff3f8b4449fe5064ede0ea526229b0fc8ea58a5031c3bf40e72d3`
are included in the final builds. Dependency freshness remains the dated
slice-start snapshot; final integration did not perform another online check.
Raw logs replace only local path prefixes with placeholders and declare that
redaction on their first line. This final set supersedes earlier integration
results for the current multipart subset; historical slice logs remain intact.
The unrestricted staged whitespace check reports Docker Compose's original
trailing spaces, for example `multipart-checks.txt:665: trailing whitespace`.
They are retained to preserve raw output. The documentation/ledger whitespace
check excludes raw evidence TXT files and passes; the diagnostic is recorded in
the ledger/documentation log.

Markdown lint uses markdownlint-cli2 v0.24.0. No repository configuration exists;
MD013 is disabled for existing long API tables, URLs and commands, and MD024 uses
`siblings_only=true` for generated category headings. Other rules retain defaults.

## Limits and next slice

SPEC-016 retains transport errors/trailers mapped to 400 `invalidParameter`,
headers-only parts requiring an additional CRLF, binary-only transfer encoding
(7bit/8bit rejected), MIME cap accounting, local contentType precedence and final
MD5/empty-object/equal/conflicting/unknown checksum declarations. Malformed final
JSON uses `parseError`; these exact local errors lack real-service differential
evidence. Shared empty/duplicate token validation also reaches objects.list and
PATCH, without dedicated malformed-token HTTP tests for those routes.

IAM/ACL/authentication, encryption, advanced metadata, gzip transcoding, historical
generations, token replay, resumable and XML multipart remain unsupported. Native
amd64, remote CI, Ruby/Rails, real-service differential tests, every fsync crash
boundary, power-loss durability and RSS/cgroup/CPU/latency measurements remain
unverified. Local arm64 containers are native; amd64 containers use emulation.

The next slice is the roadmap's public/private access work: anonymous access,
minimal strict principals/roles, public grants, denied requests and public URL
construction. Resumable transfer remains separate P3 work.
