# Dependency freshness schedule and rerun

This record covers DEP-005 and DEP-006. It was gathered read-only on
2026-11-13 with `gh run view` and one local freshness run.

## Weekend scheduled runs

The `Dependency freshness` workflow (`.github/workflows/dependency-check.yaml`)
ran on the default branch on both days of one weekend:

| Run | Day (UTC) | Event | Conclusion | Cause |
| --- | --- | --- | --- | --- |
| 42363473094 | Saturday 2026-11-07 | `schedule` | failure | The check reported `maven:com.google.cloud:google-cloud-storage: UPDATE_AVAILABLE current=2.77.0 latest=2.78.0` and a newer protobuf compiler, so it stopped as designed |
| 42471562897 | Sunday 2026-11-08 | `schedule` | success | Every checked input was current at commit `c09d27b3` |

A failed scheduled run that names an available update is the detection working,
not a broken check. The 2026-11-12 failure (run 43208764232) came from a 60-second timeout fetching
`https://proxy.golang.org/google.golang.org/api/v2/@v/list` (`exit status: 28`),
a transient network error that the next scheduled run does not inherit.

## Local rerun

`just check-dependencies` on 2026-11-13 at commit `98e75436` exited 0. It
reported 132 inputs as `CURRENT` and none as `UPDATE_AVAILABLE` or
`CHECK_FAILED`.

This rerun replaces the 2026-10-24 failure recorded in
[dist-release-notes-dependencies.txt](dist-release-notes-dependencies.txt).
That failure reported a newer `distroless/cc-debian13` runtime digest. The
runtime image moved to `scratch` on 2026-11-05, so distroless is no longer an
input.
