# Verification: reference comparison on the current bucket contracts

<!-- markdownlint-configure-file {"MD013": false} -->

This records the run of `scripts/compare-reference-emulator.sh` on 2026-11-13, on macOS arm64 with Go 1.27.4 and Rust from `rust-toolchain.toml`. The [raw output](reference-comparison.txt) keeps every request, selected header, exact response body and state probe; local path prefixes are replaced with placeholders.

## Result

| Item | Value |
| --- | --- |
| Command | `GCSM_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server bash scripts/compare-reference-emulator.sh` |
| Exit status | 0 |
| Source | commit `8861b935`, comparison script blob `1df42528` |
| Reference | fake-gcs-server v1.56.1, commit `896ece51`, archive SHA-256 `13fa1ed9…94dc`, filesystem and memory backends |
| gcs-mimic targets | `native` and `fake-gcs-server` profiles |
| Requests | 302 per target, 1,208 raw records |
| Assertions | PATCH (reference, mimic), FIELDS-ETAG, PUT (reference, mimic) and HOST (189 records) passed; every target's created and fetched bucket bodies are equal |

The `fake-gcs-server` profile answers 43 of the 302 requests with another status than the reference memory backend; the two reference backends differ in 6. The contracts list these differences as intentional, and the assertions above pin them.

## What was broken before this run

From late October the mimic assertions no longer described the bucket contracts, so the script exited 1 at the mimic PATCH assertion and never reached the PUT, fields and host checks. The ledger still recorded the PATCH comparison as `PASS` from 2026-10-23. The same failure occurs on the source before the 0.20.0 changes (`7016b7ee`), and up to that point the status differences between the reference and the profile are the same on both sources.

| Assertion | Stale expectation | Contract since |
| --- | --- | --- |
| PATCH and PUT with `cors`, `lifecycle`, `retentionPolicy` or `iamConfiguration` set to null | 400 unsupported | The settings became writable, 2026-10-23 onward; null clears them |
| PATCH or PUT with `predefinedAcl` or `predefinedDefaultObjectAcl`, native profile | 400 | Applied, 2026-10-31 |
| PUT with `acl` or `defaultObjectAcl` | 400 | Replaced with the metadata, 2026-10-29 |
| PATCH whose `ifMetagenerationNotMatch` fails | 412 | Empty 304, as observed on Cloud Storage |
| PATCH without a writable setting (`{}`, an unknown member, a projection query) | One revision | No revision, as observed on Cloud Storage |
| PATCH and PUT reply | Equal to the following GET | Equal except for the ACL members that the default `projection=full` adds |
| PUT that omits labels, versioning or the default event-based hold, native profile | Preserved | Cleared, because a native PUT replaces the resource (2026-11-01) |
| PUT with `generation=1`, or a null `storageClass`, native profile | 400 | Accepted: the query is ignored and the class kept |
| Malformed PUT body, native profile | `parseError` | `invalid`, as recorded on Cloud Storage |

The script now expects these contracts (`14e9d92d`). Altered copies of the recorded results, such as a native PUT that kept an omitted setting, a profile that accepted `generation=1` or a 412 where the contract has 304, make the corresponding assertion fail.

## Keeping it current

Until this run the comparison ran only by hand. The `reference-comparison` job in `.github/workflows/ci.yaml` now runs it on every push to `main` and every pull request that changes code (`66d46acf`). `just pre-push` still does not run it, because it downloads and builds the pinned reference.
