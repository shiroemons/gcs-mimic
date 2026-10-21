# Sources, provenance and unresolved contracts

Research cutoff: **2026-10-21**. Implementation and runtime conformance have not begun.

## Reproducible source snapshots

| Source | Evidence |
| --- | --- |
| [Live JSON Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) | Revision20261013;87 methods;38 schemas; SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |
| [Storage v2 protobuf](https://github.com/googleapis/googleapis/blob/master/google/storage/v2/storage.proto) |24 direct RPCs; Git blob `f56e4f3e0a0795ae8dc45f322477ee14b96d5291` |
| [Control generated Go reference](https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest/control/apiv2/controlpb) |39 full method constants, matching the current proto |
| [XML method reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-methods) |52 operation variants, with bucket metadata subresources separated |
| [FeatureConfigs](https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs) |5 v2 REST methods tracked outside the v1 Discovery snapshot |
| [Storage Insights REST](https://docs.cloud.google.com/storage/docs/insights/reference/rest) |20 REST methods, including common-service methods |
| [Storage Batch Operations REST](https://docs.cloud.google.com/storage/docs/storagebatchoperations/reference/rest) |13 REST methods, including common-service methods |
| [Release notes](https://docs.cloud.google.com/storage/docs/release-notes) |Semantic changes, preview features and deprecations beyond schema changes |

The full live Discovery was compared recursively with the Google-maintained mirror revision 20261009. Only the top-level revision and etag differed; all method, parameter and schema content matched. This comparison does not verify service behavior.

Exact Git blob identities, SHA-256 values and upstream paths for the eight extended proto/service-config snapshots are in `compatibility/upstream/source-manifest.json`. Factual extracts are packaged; upstream prose is not reproduced as documentation.

Source URLs on a moving branch may change after this date. Re-fetch and record the new identity when implementing or refreshing a slice. Do not confuse a Git blob identity with a repository commit SHA.

## Known contract questions

| ID | Question | Required action |
| --- | --- | --- |
| SPEC-001 | Atomic move eligibility for flat-namespace buckets is not established | The overview states HNS-only; omission of that restriction on the dedicated page does not prove its removal. Reconcile current compatibility tables, release notes and an authorized real-service fixture before choosing a strict rule. Never substitute copy plus delete for atomic move. |
| SPEC-002 | Folder rename precondition parameter spellings differ | Discovery uses ifSourceMetagenerationMatch/NotMatch; the dedicated page uses ifMetagenerationMatch/NotMatch. Verify accepted protocol-specific names and exact error behavior. |
| SPEC-003 | Bulk restore matchGlob versus matchGlobs discrepancy | Determine the accepted request field and any transitional aliases from current SDK schemas and authorized conformance evidence. |
| SPEC-004 | FeatureConfig bucket selectors: regex schema versus exact-name guidance | Verify the current preview restrictions. Do not infer enabled regex behavior from a field name. |
| SPEC-006 | Discovery and proto exposure do not establish launch stage or availability | Check launch stage and eligibility for less documented routes, lifecycle matchesPattern early access and preview features. Reserved satisfiesPZS/satisfiesPZI outputs must not be interpreted as real residency guarantees. |
| SPEC-007 | Shared LRO definitions need per-service applicability verification | A registered Operations service does not guarantee all five methods for every operation. Verify unsupported/cancellation behavior; Delete is not Cancel and Wait is not an unconditional completion promise. |

## Important scope boundaries

Cache acceleration, geographic redundancy, production durability, Google identity issuance, KMS execution, billing, BigQuery query execution, managed ML enrichment and external network services cannot be certified by a local emulator test. Track their local state/wire simulations separately from external services.

The Cloud Storage API is distinct from Firebase Storage APIs/Security Rules, IAM Credentials signing, Organization Policy administration, Resource Manager tags, Storage Transfer Service, Pub/Sub and Cloud Monitoring/Logging. Their effects and optional adapters belong in the ledger, but this project must not claim to implement those entire services.

Protocol fields and public APIs are a dated baseline. An inventory is not an assurance that every undocumented combination is known. New accepted cases and upstream changes become new pending work rather than silently expanding an implemented claim.

## Reference index

- https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/
- https://doc.rust-lang.org/cargo/commands/cargo-update.html
- https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html
- https://doc.rust-lang.org/cargo/reference/rust-version.html
- https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
- https://doc.rust-lang.org/edition-guide/editions/
- https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest
- https://docs.cloud.google.com/iam/docs/deny-access
- https://docs.cloud.google.com/iam/docs/deny-permissions-support
- https://docs.cloud.google.com/ruby/docs/reference/google-cloud-storage/latest/Google-Cloud-Storage
- https://docs.cloud.google.com/storage-transfer/docs/overview
- https://docs.cloud.google.com/storage/docs/access-control
- https://docs.cloud.google.com/storage/docs/access-control/iam
- https://docs.cloud.google.com/storage/docs/access-control/signed-urls
- https://docs.cloud.google.com/storage/docs/access-control/signed-urls-v2
- https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually
- https://docs.cloud.google.com/storage/docs/authentication
- https://docs.cloud.google.com/storage/docs/authentication/canonical-requests
- https://docs.cloud.google.com/storage/docs/authentication/signatures
- https://docs.cloud.google.com/storage/docs/autoclass
- https://docs.cloud.google.com/storage/docs/availability-durability
- https://docs.cloud.google.com/storage/docs/aws-simple-migration
- https://docs.cloud.google.com/storage/docs/batch
- https://docs.cloud.google.com/storage/docs/bucket-lock
- https://docs.cloud.google.com/storage/docs/bucket-relocation/overview
- https://docs.cloud.google.com/storage/docs/cloud-storage-fuse/overview
- https://docs.cloud.google.com/storage/docs/consistency
- https://docs.cloud.google.com/storage/docs/cross-origin
- https://docs.cloud.google.com/storage/docs/encryption
- https://docs.cloud.google.com/storage/docs/encryption/customer-managed-keys
- https://docs.cloud.google.com/storage/docs/encryption/customer-supplied-keys
- https://docs.cloud.google.com/storage/docs/gsutil_install
- https://docs.cloud.google.com/storage/docs/hns-overview
- https://docs.cloud.google.com/storage/docs/hosting-static-website
- https://docs.cloud.google.com/storage/docs/insights/datasets
- https://docs.cloud.google.com/storage/docs/insights/inventory-reports
- https://docs.cloud.google.com/storage/docs/insights/reference/rest
- https://docs.cloud.google.com/storage/docs/interoperability
- https://docs.cloud.google.com/storage/docs/ip-filtering-overview
- https://docs.cloud.google.com/storage/docs/json_api/v1/buckets
- https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/restore
- https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/create
- https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/delete
- https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/get
- https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/list
- https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/patch
- https://docs.cloud.google.com/storage/docs/json_api/v1/folders/rename
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/folders.locations/getIntelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/folders.locations/updateIntelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/organizations.locations/getIntelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/organizations.locations/updateIntelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/projects.locations/getIntelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/projects.locations/updateIntelligenceConfig
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings.revisions/list
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings/get
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings/list
- https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings/summarize
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/bulkRestore
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/move
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/restore
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/rewrite
- https://docs.cloud.google.com/storage/docs/json_api/v1/objects/viewFullContext
- https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes
- https://docs.cloud.google.com/storage/docs/lifecycle
- https://docs.cloud.google.com/storage/docs/locational-endpoints
- https://docs.cloud.google.com/storage/docs/managed-folders
- https://docs.cloud.google.com/storage/docs/metadata
- https://docs.cloud.google.com/storage/docs/monitoring
- https://docs.cloud.google.com/storage/docs/object-contexts
- https://docs.cloud.google.com/storage/docs/object-enrichments/configure-object-enrichments
- https://docs.cloud.google.com/storage/docs/object-enrichments/manage-object-enrichments
- https://docs.cloud.google.com/storage/docs/object-enrichments/models
- https://docs.cloud.google.com/storage/docs/object-holds
- https://docs.cloud.google.com/storage/docs/object-lock
- https://docs.cloud.google.com/storage/docs/object-versioning
- https://docs.cloud.google.com/storage/docs/objects
- https://docs.cloud.google.com/storage/docs/org-policy-constraints
- https://docs.cloud.google.com/storage/docs/performing-resumable-uploads
- https://docs.cloud.google.com/storage/docs/public-access-prevention
- https://docs.cloud.google.com/storage/docs/pubsub-notifications
- https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket
- https://docs.cloud.google.com/storage/docs/rapid/rapid-cache
- https://docs.cloud.google.com/storage/docs/rapid/use-objects-in-zonal-buckets
- https://docs.cloud.google.com/storage/docs/reference/libraries
- https://docs.cloud.google.com/storage/docs/reference/mcp
- https://docs.cloud.google.com/storage/docs/reference/rpc/google.longrunning
- https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.control.v2
- https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2
- https://docs.cloud.google.com/storage/docs/reference/rpc/storage-control-operations
- https://docs.cloud.google.com/storage/docs/regional-endpoints
- https://docs.cloud.google.com/storage/docs/release-notes
- https://docs.cloud.google.com/storage/docs/request-endpoints
- https://docs.cloud.google.com/storage/docs/request-preconditions
- https://docs.cloud.google.com/storage/docs/requester-pays
- https://docs.cloud.google.com/storage/docs/resumable-uploads
- https://docs.cloud.google.com/storage/docs/samples/storage-s3-sdk-list-objects
- https://docs.cloud.google.com/storage/docs/soft-delete
- https://docs.cloud.google.com/storage/docs/storage-intelligence/findings/findings-reference-catalog
- https://docs.cloud.google.com/storage/docs/storagebatchoperations/reference/rest
- https://docs.cloud.google.com/storage/docs/storagebatchoperations/reference/rest/v1/projects.locations.jobs
- https://docs.cloud.google.com/storage/docs/tags-and-labels
- https://docs.cloud.google.com/storage/docs/uniform-bucket-level-access
- https://docs.cloud.google.com/storage/docs/use-object-contexts
- https://docs.cloud.google.com/storage/docs/use-tags-for-soft-delete
- https://docs.cloud.google.com/storage/docs/xml-api/get-bucket-metadata
- https://docs.cloud.google.com/storage/docs/xml-api/get-hmac-list
- https://docs.cloud.google.com/storage/docs/xml-api/get-object-encryption
- https://docs.cloud.google.com/storage/docs/xml-api/get-object-retention
- https://docs.cloud.google.com/storage/docs/xml-api/post-bucket
- https://docs.cloud.google.com/storage/docs/xml-api/post-hmac-create
- https://docs.cloud.google.com/storage/docs/xml-api/post-hmac-delete
- https://docs.cloud.google.com/storage/docs/xml-api/post-hmac-update
- https://docs.cloud.google.com/storage/docs/xml-api/post-object-complete
- https://docs.cloud.google.com/storage/docs/xml-api/post-object-forms
- https://docs.cloud.google.com/storage/docs/xml-api/post-object-multipart
- https://docs.cloud.google.com/storage/docs/xml-api/post-object-resumable
- https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-create
- https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata
- https://docs.cloud.google.com/storage/docs/xml-api/put-object-compose
- https://docs.cloud.google.com/storage/docs/xml-api/put-object-copy
- https://docs.cloud.google.com/storage/docs/xml-api/put-object-multipart
- https://docs.cloud.google.com/storage/docs/xml-api/put-object-retention
- https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload
- https://docs.cloud.google.com/storage/docs/xml-api/reference-methods
- https://docs.cloud.google.com/storage/docs/xml-api/reference-status
- https://docs.cloud.google.com/storage/quotas
- https://docs.cloud.google.com/vpc-service-controls/docs/supported-products
- https://docs.docker.com/build/building/best-practices/
- https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference
- https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
- https://firebase.google.com/docs/emulator-suite/connect_storage
- https://github.com/fsouza/fake-gcs-server/blob/main/README.md
- https://github.com/fsouza/fake-gcs-server/blob/main/fakestorage/server.go
- https://github.com/googleapis/google-api-python-client/blob/main/googleapiclient/discovery_cache/documents/storage.v1.json
- https://github.com/googleapis/google-cloud-rust
- https://github.com/googleapis/googleapis/blob/master/google/cloud/location/locations.proto
- https://github.com/googleapis/googleapis/blob/master/google/cloud/storagebatchoperations/v1/storage_batch_operations.proto
- https://github.com/googleapis/googleapis/blob/master/google/cloud/storagebatchoperations/v1/storagebatchoperations_v1.yaml
- https://github.com/googleapis/googleapis/blob/master/google/cloud/storageinsights/v1/storageinsights.proto
- https://github.com/googleapis/googleapis/blob/master/google/cloud/storageinsights/v1/storageinsights_v1.yaml
- https://github.com/googleapis/googleapis/blob/master/google/storage/control/v2/storage_control.proto
- https://github.com/googleapis/googleapis/blob/master/google/storage/v2/storage.proto
- https://github.com/googleapis/nodejs-storage/blob/main/src/storage.ts
- https://github.com/googleapis/python-storage/blob/main/google/cloud/storage/_helpers.py
- https://github.com/googleapis/storage-testbench
- https://guides.rubyonrails.org/active_storage_overview.html
- https://rust-lang.github.io/rustup/overrides.html
- https://storage.googleapis.com/$discovery/rest?version=v1
- https://tokio.rs/tokio/tutorial/channels
- https://www.sqlite.org/wal.html
