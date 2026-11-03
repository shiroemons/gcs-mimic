# Upload replay encoding migration

The next release stores upload and copy replay fingerprints using a versioned
canonical binary encoding. It does not depend on Rust diagnostic formatting.

The v2 encoding preserves the existing digest for the default metadata-map merge
mode. Replacing the map adds an explicit `metadata-map-mode` / `replace` tag,
so otherwise identical merge and replacement requests cannot replay each other.
All metadata fields are encoded explicitly and covered by identity-difference
tests; diagnostic Debug output is not part of this compatibility contract.

Retained requests created by an older binary can return 400 on retries after
an upgrade, until their original ten-minute retention window expires. These
conflicts never execute a second mutation. Check the published object before
starting a new invocation. No catalog schema migration is required.
