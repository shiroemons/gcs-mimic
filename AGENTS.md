# gcs-mimic implementation rules

gcs-mimic is an independent Google Cloud Storage emulator written in Rust. It targets local development and integration tests, with fake-gcs-server-inspired ergonomics and independently verified Google API compatibility.

## Scope and workflow

- Read `README.md`, `docs/en/design.md`, `docs/en/roadmap.md`, `docs/en/compatibility-policy.md`, `docs/en/dependency-policy.md`, `docs/en/getting-started-spec.md`, `compatibility/features.json`, and the requested feature's sources before editing.
- Implement one small end-to-end feature slice at a time. Follow the requested IDs; if none are supplied, choose the next unblocked slice from the roadmap and announce its concrete scope.
- Inspect existing code and preserve unrelated changes. Add the smallest required dependency, not an entire milestone by implication.
- Use current official Google documentation, Discovery schemas and googleapis protobuf definitions as the contract. fake-gcs-server and storage-testbench are comparison implementations, not authoritative specifications.
- Pin upstream source revisions, content digests, protocol versions and SDK versions. Reconcile `compatibility/gaps.json` before implementing affected features. Discovery presence alone does not establish GA availability.

## Latest stable dependencies

- Use the latest verified stable Rust, supported edition, and non-yanked non-prerelease releases of chosen libraries. Check official releases at bootstrap, slice start and before release; do not use remembered versions as the baseline.
- Adopt breaking major library updates through deliberate migrations. A normal `cargo update` within old manifest ranges is not proof that every direct dependency is current.
- Record the verified exact Rust toolchain, committed application and example lockfiles, and actual Docker digests. Reproducible pins must advance through tested updates.
- Implement daily update detection for the actual dependency ecosystems, compiler and container inputs. A temporary upstream incompatibility requires visible failing evidence and a review deadline; it does not satisfy the freshness requirement.
- Use pinned release CI plus a current-stable lane. Do not add a nightly requirement just to use an unstable update command. Follow `docs/en/dependency-policy.md`.

## Compatibility and evidence

- `compatibility/features.json` is the sole implementation-status source of truth. Generate the canonical English matrix from it. Optional Japanese companion views use the same status; never maintain two independent states.
- Track API operations, semantic behavior and project requirements separately. Track target fidelity, achieved fidelity, and implementation status separately.
- A route stub, a successful status code, accepted-but-ignored setting, permissive authentication, or metadata round trip is not full implementation.
- Before implementing a row, decompose its request fields, response fields, permission checks, side effects, error cases and feature interactions into acceptance cases. Record the exact supported subset.
- Mark `IMPLEMENTED` only after all scoped acceptance cases pass with recorded evidence. Keep incomplete work `PARTIAL`; keep Google infrastructure simulations labeled `LOCAL_SIMULATION` even when their local implementation is complete.
- Unknown fields must follow the relevant upstream contract. Do not reject every unknown field by policy, and do not return false success for recognized but unsupported semantics.
- Do not claim test execution or performance improvements without actual results. Never remove or weaken a negative test to get a green status.

## Core design

- Keep JSON, XML and gRPC adapters separate from shared object, bucket, authorization and state-transition logic.
- Use bounded streaming for all object bodies, copies, rewrites, composition and multipart parsing. Do not collect entire payloads or the whole object catalog in memory.
- Enforce generation and metageneration conditions atomically with the mutation. Reads and writes must observe the selected generation consistently.
- Use opaque blob IDs rather than filesystem paths derived from object names. Preserve raw encoded paths until protocol-specific decoding/canonicalization is complete.
- Default to disk-backed blobs plus a transactional metadata catalog. SQLite plus filesystem rename is not one atomic transaction: define a commit/recovery protocol and validate crash boundaries.
- Keep database transactions short. Bound concurrency, queues, database caches and blocking/CPU workers. An async function does not make blocking work harmless.
- Inject a clock and explicit maintenance scheduler for expiration, lifecycle, retention and propagation scenarios. Avoid polling loops when no work is due.
- Keep standard immutable-object semantics separate from Rapid appendable-object state machines.

## Rust implementation guardrails

Run `cargo xtask lint` (also run by `just check` and CI) before pushing; any finding in production code fails it. Silence a finding only with a written reason; the checks and exception syntax are in `docs/en/development.md` ("Production lint gate"). Each rule states why.

- No `unwrap()` in production; `expect()` only for a proven invariant, under `#[expect(clippy::expect_used, reason = "...")]`. A panic reachable from a request drops the connection or crashes a task.
- Silence lints with `#[expect(lint, reason = "...")]`, never `#[allow]`. An expectation fails once its lint stops firing, so a stale exception cannot outlive its reason.
- Isolate panics per connection and background task. One faulty request must not stop the listener or deny service to other clients.
- Spawn tasks and OS threads only through `src/task.rs` (`spawn_supervised`, `spawn_thread`, `catch_job` per worker job); Clippy's `disallowed-methods` rejects direct spawns. A dropped `JoinHandle` or a dead thread hides the panic or error.
- Recover from poisoned locks or fail visibly through health checks and errors. A poisoned mutex otherwise turns one panic into a silent permanent outage.
- Return typed errors that keep the cause as `source()`: no new `Result<_, String>`, bare-`String` failure variants such as `Backend(String)`, or `map_err(|e| Variant(e.to_string()))`. Stringified errors lose the cause chain; a variant naming rejected client input, such as `InvalidTopic(String)`, may keep its message.
- Map storage errors through the one `fn unavailable` in `src/storage/failure.rs` or a `From` conversion. Per-module copies drift apart in status codes and messages.
- Keep functions within 120 lines (`clippy.toml`), group request context in a struct such as `server::context::RequestCtx`, and never silence `clippy::too_many_arguments`. Giant handlers and long parameter lists hid untestable, inconsistent behavior; a linear protocol or a single dispatch `match` may exceed the threshold under a reasoned `#[expect(clippy::too_many_lines)]`.
- Mirror the module tree in the file tree; no `#[path]`. Diverging trees make code hard to find and privacy hard to reason about.
- Default to `pub(crate)` and compile test-only helpers under the tests' `cfg`. Every `pub` item is API surface that blocks refactoring.
- Make a `bool` parameter a two-variant enum when it names a mode at call sites, and carry the enum end to end. A literal `true` hides its meaning; a helper forwarding a stored or wire boolean may keep it under a reasoned `#[expect(clippy::fn_params_excessive_bools)]`.
- Use `prepare_cached` with static SQL on request paths: Clippy rejects rusqlite's `Connection::execute`, `query_row`, `query_one` and `prepare`, and `text::format_sql` rejects built SQL. Re-preparing per request wastes CPU and built SQL invites injection. Code that no API request reaches, directly or through a wakeup it triggers (open, migration, startup, background-only workers), keeps them under a reasoned `#[expect(clippy::disallowed_methods)]`; the maintenance sweep is not such code, because resumable, rewrite and derived requests wake it.
- Run blocking I/O and CPU-heavy work only on the bounded blocking pools. Blocking an async worker stalls every connection it serves.
- Stream object bodies with bounded buffers. Unbounded buffering makes memory grow with request size.
- Feed producer-driven bodies through `crate::server::chunk_body`, never `http_body_util::Channel` (disallowed in `clippy.toml`). `Channel` can drop the last queued frame when the sender drops mid-poll, truncating a response.
- Flush a `tokio::fs::File` before `sync_all()` or `sync_data()`. tokio finishes the last write in the background and the sync does not report its error, so a failed tail write would publish a short file as durable.
- Test each changed durable write path for a failed write and a failed flush or sync, and each new publish, rename or fsync step for a crash and a `Fault::PowerLoss` snapshot followed by restart and recovery, through the seam in `src/storage/catalog/fault.rs` (`objects::make_durable`, `objects::sync_dir`). Ordinary tests never hit these failures; the failed-tail-write bug surfaced only through a flaky test. A `Fault::Snapshot` crash keeps unsynced bytes like a process abort, so only a power-loss snapshot makes a missing fsync fail. The model covers only seamed steps: renames, unlinks and new-file entries made outside the seam (such as the restore-to-staging renames in recovery) are not modeled, and a truncation counts only where it calls `fault::truncated`.

## Access and URLs

- Public URL construction never changes IAM or ACL permissions. `allAuthenticatedUsers` is not anonymous public access.
- Explicitly separate `permissive` and `strict` authentication profiles. Strict mode uses local test principals and rejects unsupported authentication behavior instead of bypassing it.
- Model IAM, ACLs, default ACLs, uniform bucket-level access and public access prevention together. Valid signed URLs remain usable under public access prevention when the signer has the required permission.
- Signed URLs use XML-compatible endpoints. Sign the final externally visible host, path, query and required headers; do not rewrite the host after signing in strict mode.
- JSON and XML CORS behavior differs. CORS is not authorization.
- Distinguish listener address, advertised API endpoint, public object endpoint, trusted proxy configuration and allowed hosts. Keep emulator-only helper/admin routes outside Google API namespaces.

## Verification and resource use

- Use hermetic wire-contract tests and version-pinned official SDK tests. Prioritize Go and Ruby/Rails Active Storage, then Node.js, Python and Rust; expand to other SDKs according to the ledger.
- Exercise positive, negative, boundary, concurrency, recovery and resource cases that are relevant to the slice. Do not create redundant tests for cosmetic changes.
- Default tests must not contact Google Cloud or the metadata server. Real-cloud differential tests require an explicitly opted-in test project/bucket, budget and cleanup policy.
- Never run irreversible real-bucket retention locking against a user or production bucket as an incidental test. Emulator-local lock tests remain ordinary offline tests.
- gcs-mimic targets local-development scale (up to about a thousand objects per bucket); delivery speed takes priority over benchmarking. Do not build benchmark harnesses, repeated-sample baselines or regression gates for ordinary slices. Measure only when a slice's purpose is performance, with one short run at that scale, and record the result as a fact rather than a gate.
- When measuring resources, keep process RSS and cgroup memory separate and compare only equivalent profiles and workloads. Bounded streaming and memory that does not grow with object count remain design requirements enforced by code and review.
- Do not weaken checksums, authorization, synchronization or durability to improve a benchmark without an explicit documented profile and tradeoff.

## Language and delivery

- English is primary for code, identifiers, comments, commit messages, issue/PR text, logs, CLI help and canonical documentation.
- Implementation prompts and communication with the project owner are Japanese. English remains primary for repository documentation. Requested Japanese user documentation lives in `docs/ja`; maintain the same feature IDs, status and supported scope as English. Generate both feature matrices from the same ledger. Japanese notes under `planning/notes-ja` are historical planning material, not the user guide or a second status source.
- Use focused English Conventional Commits, for example `feat(objects): enforce generation preconditions` and `docs: explain signed URL endpoints`.
- At the end of a slice, report changed feature IDs, verified behavior, commands/results, limitations, ledger updates and the next slice.
- Follow session authorization for commits, pushes, PRs and releases. These project rules do not by themselves authorize publishing a package or creating billable cloud resources.
