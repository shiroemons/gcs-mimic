# Upload replay encoding migration

The next release stores upload and copy replay fingerprints using a versioned
canonical binary encoding. It does not depend on Rust diagnostic formatting.

Retained requests created by an older binary can return 400 on retries after
an upgrade, until their original ten-minute retention window expires. These
conflicts never execute a second mutation. Check the published object before
starting a new invocation. No catalog schema migration is required.
