# Read-only filesystem import inspection evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-10-24 at runtime source `66dfab5`, rebased onto main
`f2df2f1` including startup phases and public-origin configuration, Rust 1.99.1,
edition 2024. Scope: DESIGN-072 `source-filesystem-reader`,
`source-integrity-report` and `source-name-scope`. The broader DESIGN-062,
DESIGN-072, DESIGN-073 and AUDIT-014 rows remain PARTIAL. No destination
publication, schema change, IAM grant, memory export or completed migration
is claimed. The contract is [source storage](../slices/source-storage-contract.md#executable-dry-run).

## Executed acceptance

The native macOS ARM64 focused suite and the pinned Linux ARM64 suite each
passed 15 integration tests, 3 xattr tests, 7 codec/property/injection tests,
2 hash/hex tests and 1 traversal snapshot test. Linux used the Rust image index
`sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6`,
network disabled, a read-only checkout and a separate temporary target tree.
The [Linux stdout](import-dry-run-linux.txt) contains all executed commands'
test results and successful Linux license-notice generation.

| Behavior | Named executable evidence |
| --- | --- |
| Source bytes, metadata sidecar bytes, permissions, mtime and ctime unchanged; no destination created; metadata-only zero body reads versus exactly 5 bytes hashed | `sidecar_inspection_is_read_only_and_distinguishes_body_verification` |
| Same descriptor xattr/body input, unchanged xattr bytes and file observations, missing attributes and mixed-mode rejection | `xattr_snapshot_verifies_bodies_without_mutating_source_attributes` |
| Linux four-argument/macOS six-argument descriptor ABI, missing versus unsupported/permission errors, original errno, zero/exact-cap/cap-plus-one lengths | `metadata_errno_retains_platform_distinctions`, `metadata_lengths_include_exact_cap_and_empty`, `descriptor_xattr_reads_are_bounded_and_read_only` |
| Missing/orphan bodies and bucket attributes, malformed/duplicate/unknown/wrong-type JSON, duplicate metadata keys, nonzero sub-microsecond times, unsupported ACL/CustomTime/retention and hash mismatches; relative fields and remedies | `corrupt_records_are_all_reported_with_relative_fields_original_values_and_remedies` |
| Fixed 65536-byte body chunks, terminal 1-byte chunk, same-size replacement and truncation detection, typed inode/ctime-only drift classification | `body_stream_bounds_chunks_and_detects_same_size_replacement_and_truncation` |
| At most 256 combined records/findings per page, multiple pages, counted regular OS detritus, preserved stored `.DS_Store`, capped entry traversal, 65536-byte metadata acceptance/65537 rejection, inclusive 5-byte body cap and valid zero-byte body | `metadata_and_body_limits_empty_files_os_detritus_and_page_counts_are_explicit` |
| Required dry-run/options, no listener/ready/catalog creation, helper help and duplicate rejection; closed report joins the bounded OS worker | `cli_never_starts_serve_and_requires_explicit_read_only_mode`, `broken_report_output_joins_the_bounded_worker_without_destination_writes` |
| Canonical overlap in both directions, absent destination, bounded depth and symlink rejection | `overlap_and_walk_fail_closed_without_creating_destination` |
| Duplicate decoded buckets, invalid percent escapes, suffix collisions, Windows-source/xattr rejection and ambiguous Unix-materialized Windows backslash rejection | `escaped_bucket_aliases_invalid_escapes_suffix_collisions_and_platform_rules_block` |
| Upstream `a//b`, `a/./b`, `a/../b`, `x/../../other/y` writes show only resulting disk names; original API names remain unverifiable | `normalized_source_names_are_reported_without_inventing_original_names` |
| Arbitrary Unicode escape round trips, valid bucket-name round trips and panic-free arbitrary timestamps; exact negative-epoch/offset/fraction/sentinel examples | `escaped_names_round_trip`, `valid_bucket_names_round_trip`, `arbitrary_times_never_panic`, `exact_times_offsets_zero_and_negative_epoch` |
| Both digests resume from serialized state across a chunk boundary and reject invalid state | `checkpoint_resumes_both_digests` |
| Lowercase hexadecimal preserves leading zeroes | `encode_lower_preserves_leading_zeroes` |
| Native class/header validation rejects REGIONAL, UTF-8 disposition, tabbed cache-control and overlong contentLanguage; field/reason assertions identify each cause | `native_storage_class_and_fixed_metadata_validation_matches_import_eligibility` |
| Unsupported xattr absence emits exactly one warning per sidecar object, retains eligibility and explicitly clears mixed-format absence verification; ordinary xattr object has exactly two attribute reads without a detritus probe | `unsupported_xattr_inspection_warns_once_per_sidecar_object`, `ordinary_xattr_objects_do_not_get_a_detritus_probe` |
| Discovered counts include blocked candidates; invalid parents suppress Object/Directory records and eligible counts while corrupt JSON, missing sidecars, mismatched bodies and directory warnings remain visible without repeated parent findings. Alias accounting uses names from the Walker snapshot regardless of type and blocks aliases before emission | `discovered_counts_and_eligible_records_agree_for_buckets_and_objects`, `duplicate_bucket_aliases_never_emit_eligible_child_records`, `blocked_bucket_reports_child_corruption_without_emitting_records`, `invalid_parent_names_and_attributes_keep_child_diagnostics`, `root_snapshot_is_shared_with_the_walk` |
| Versioned binary SHA-256 export changes for directory/skip additions, selected attribute bytes, size with restored mtime, nanosecond mtime, source platform and body limit; stat-only replacement keeps the export digest stable. Incomplete/blocked scans expose no digest; successful digests are 64 lowercase hexadecimal characters | `export_fingerprint_tracks_layout_and_skips_but_separates_stat_drift`, `sidecar_inspection_is_read_only_and_distinguishes_body_verification` |

`seed` (13 native macOS tests) and `object_metadata_http` (5 tests) also passed after the
canonical-path comparison and metadata validation extraction. API error and
validation behavior is retained. The Linux-only non-UTF-8 seed case remains
present but is excluded by its existing platform configuration on macOS;
the two additional native seed cases cover startup signal handling from main.
Every original import integration test remains;
negative fixtures now assert each expected path/field and reason. Body cap
acceptance/rejection uses a fresh fixture independent of oversized sidecars;
missing xattr also has a separate fixture from mixed metadata modes.

`read_source_metadata` takes only `SourceMetadataBuffer` and performs exactly
one `fgetxattr` call per invocation, with no query/retry. The private allocation
is fixed at 65537 bytes; only a returned length through 65536 is borrowed.
Its compile-fail doctest rejects an arbitrary smaller array. Linux filesystems
can reject cap-sized fixture writes at their own lower ceiling; the test
retains that explicit OS error and separately checks reader length boundaries.
macOS accepts the actual exact-cap and cap-plus-one fixture writes.
All new parser/helper modules deny Clippy unwrap/expect; test allowances are
local to test modules. All new unsafe calls, including the test-only safe
creation wrapper, are physically inside `src/fsutil.rs` under its documented
module exception and per-call safety comments. Xattr import behavior is a
library unit test using `cfg(test)`; no unsafe source or allowance enters the
integration-test crate. Workspace lint levels remain unchanged. Randomized
test source directories have a Drop cleanup guard. Unsupported fixture xattrs
print an explicit capability reason, retaining deterministic error/length tests.
The recorded Linux/macOS executions exercised xattrs. Linux reported its own
ENOSPC ceiling for the 65536-byte write and E2BIG for 65537 bytes; neither is
reported as a successful source xattr read.

Object hashing checkpoints expose `to_bytes()` and `from_bytes()` for an
allocation-free 101-byte format: format byte, big-endian read-byte count,
88-byte MD5 state and big-endian CRC32C. Format 1 pins the md-5 0.11 state ABI; an
incompatible upgrade must bump the format and reread bodies. A fixed 65537-byte
input has an exact 101-byte golden vector, detecting ABI changes at
unchanged length. The state is private; `from_bytes()` alone validates length,
format and read count against the MD5 block counter/buffer position.
The compile-time size assertion freezes the MD5 ABI; field offsets derive
from that size. `resume(state, offset)` consumes validated state and rejects
a mismatched persisted offset before returning the hasher.
The selected object-size admission bounds the infallible incremental counter.
Export/stat/body
summary SHA-256 contexts use the versioned, length-prefixed binary encoding
documented in the contract, without JSON struct serialization. Body buffers
are reused per worker and absent during metadata-only scans. Rechecks compare
xattr bytes in SourceMetadataBuffer and sidecars in fixed chunks, then check
the opened-file/path observations once. Corruption and traversal limits carry
typed fields/reasons and remedies rather than collapsing into generic I/O.
Remedies exhaustively match a typed object-field enum. Invalid class values
have object-specific reasons and supported-value advice; regional/zonal class
values have distinct unsupported-placement advice. Depth remedies include the
selected limit. Static remedies are borrowed, allocating only dynamic text.
API PATCH and PUT wire assertions preserve 501/notImplemented for non-ASCII
contentDisposition and 400/invalidArgument for tabbed cacheControl.

## Final checks

The final `PATH="$HOME/.cargo/bin:$PATH" just check` passed at runtime source
`66dfab5`: **750 Rust tests across 38 result groups**, including the buffer
compile-fail doctest, with no test failures or ignored tests. The same run
passed fmt, warning-free workspace/all-target Clippy, compatibility checking
and the shell/container-release-script checks. See the
[final stdout](import-dry-run-checks.txt). This is the final execution after
the validation, diagnostic, fingerprint, counter, alias, hash-resume and
test-fixture changes, after rebasing onto the final main changes.

`cargo xtask compatibility render` and `cargo xtask compatibility check` pass
with 591 feature rows, 401 parameter locations and 357 schema fields. Both
generated matrices remain unchanged because broad statuses/fidelity are
unchanged. A JSON comparison excluding DESIGN-072 and DESIGN-073 confirms
that all other feature rows and delivery-plan content are unchanged.
The comparison uses main `f2df2f1`, and DESIGN-072 retains its startup evidence.
Markdownlint cli2 0.24.0 / markdownlint 0.42.0 passes for the contract, evidence
and Japanese storage guide. Tanteki textlint and semantic review pass for
that guide; its canonical feature IDs and the algorithm name SHA-256 are
narrow stock-boundary false-positive exceptions, with other rules enabled.
`git diff --check` and local-path scans pass; the only tracked repository-wide
path-pattern match is the preexisting scan command in the planning rules.

The native notice test requires the pinned rustup toolchain's copyright and
license resources. The shell's Homebrew rustc has the same version but lacks
those files; selecting the existing rustup tools resolves that environment
failure without changing/skipping the test. Linux notice generation passed.

## Limits and handoffs

The source writer must be stopped or an immutable snapshot provided. Ordinary
reads can update atime. A preexisting truncation is detectable by stored hashes
only during `--verify-bodies`; metadata-only/stat inspection does not verify
bytes. Normalized-away names, overwritten aliases and earlier filesystem
generations cannot be reconstructed. Source ETags are report-only; generated
native identities are not source compatibility claims. Native Windows runtime
and file-ID/ctime observation remain unverified, with absent optional identity
fields. Windows-target Clippy with the correct rustup standard library exits
101 before application analysis because the MinGW C compiler is unavailable;
no Windows result is claimed. The retained error is:

```text
error occurred in cc-rs: failed to find tool "x86_64-w64-mingw32-gcc": No such file or directory (os error 2)
```

The Shared state receiving record in
[onboarding-state-contract.md](../slices/onboarding-state-contract.md#accepted-import-requests)
now accepts the staging budget, persisted observation and review handoffs.
Their runtime cases and `import-overlap`/`import-resume` benchmarks remain
PENDING. The coordinator confirmed that no time/CPU/RSS benchmark is required
for this dry-run slice; fixed buffers/pages and observed bytes hashed are
tested. No elapsed-time or resource improvement is claimed. Existing storage
hash loops remain for `shared-blob-id`, whose owner is also requested to
consolidate the older storage/JSON/authentication hex encoders with
`hashing::encode_lower` used by import. The Shared state storage owner is
requested to adopt `source_tree::is_os_metadata` for regular entries, and
`import-seed-adapter` is requested to migrate seed walking to Walker with
dedicated workers and local publication. Those migrations retain their own
acceptance and native ignore-policy change; this slice changes no storage
recovery or seed ignore behavior. Seed compares its already-async-canonicalized
paths directly, without duplicate synchronous filesystem calls.
Bucket-name decoding/pending-state consolidation is deferred to
`import-filesystem` with its reason in the slice contract and ledger; it does
not defer invalid-parent diagnostics or record suppression.
No SDK suite, server journey or application runtime container build was needed
for the offline helper; the Linux test container is the scoped platform check.

The [freshness output](import-dry-run-dependencies.txt) verifies libc 0.2.191
as CURRENT. It also detects a newly available rusqlite 0.41.0 release and
then fails with HTTP 403 while fetching the upload-artifact release, exit 2.
The slice-start check had already found the existing distroless digest update.
These are visible dependency-maintenance follow-ups outside this slice;
libc's approved direct promotion changes no production package version.
