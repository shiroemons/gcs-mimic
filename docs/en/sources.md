# Sources, provenance and unresolved contracts

<!-- markdownlint-configure-file {"MD013": false} -->

Initial research cutoff: **2026-10-21**. Later slice-specific source checks and
executed conformance evidence are recorded below and in the feature ledger.

## Reproducible source snapshots

| Source | Evidence |
| --- | --- |
| [Live JSON Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) | Revision20261013;87 methods;38 schemas; SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |
| [Storage v2 protobuf](https://github.com/googleapis/googleapis/blob/master/google/storage/v2/storage.proto) | 24 direct RPCs; Git blob `f56e4f3e0a0795ae8dc45f322477ee14b96d5291` |
| [Control generated Go reference](https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest/control/apiv2/controlpb) | 39 full method constants, matching the current proto |
| [XML method reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-methods) | 52 operation variants, with bucket metadata subresources separated |
| [FeatureConfigs](https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs) | 5 v2 REST methods tracked outside the v1 Discovery snapshot |
| [Storage Insights REST](https://docs.cloud.google.com/storage/docs/insights/reference/rest) | 20 REST methods, including common-service methods |
| [Storage Batch Operations REST](https://docs.cloud.google.com/storage/docs/storagebatchoperations/reference/rest) | 13 REST methods, including common-service methods |
| [Release notes](https://docs.cloud.google.com/storage/docs/release-notes) | Semantic changes, preview features and deprecations beyond schema changes |

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
| SPEC-017 | Strict authentication error details and missing-bucket authorization are partly undocumented | The `authError` message/location, longer 403 messages, `WWW-Authenticate`, missing-bucket 404/403, `projection=full` permissions and the 412-before-403 overwrite order need authorized real-service evidence. Local choices are listed in the [strict authentication contract](slices/strict-auth.md). |

## Bucket metadata PATCH source check

Checked on **2026-10-23**: [PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch),
[Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
[partial updates](https://docs.cloud.google.com/storage/docs/json_api#partial-updates),
[label updates](https://docs.cloud.google.com/storage/docs/using-bucket-labels),
[storage classes](https://docs.cloud.google.com/storage/docs/storage-classes) and
[status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes).
The [contract](slices/bucket-metadata-patch.md) records Discovery revision
`20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`,
and supplementary googleapis commit `14a161a98ef045be80c12ab69d924504a756f109`.
The [executed comparison](evidence/bucket-metadata-patch-comparison.md) pins
fake-gcs-server v1.56.1 and the official Go serializer revisions/digests.
The gcp-storage-emulator v2026.07.19
[bucket handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py)
was source-reviewed: `_patch_bucket` assigns the supplied labels map and ignores
whole-field null, without per-label merge/deletion logic. No execution claim is
made for that product. SPEC-BUCKET-PATCH and SPEC-017 retain provider questions;
no reference implementation overrides the official contract.

## Strict authentication sources

Checked on 2026-10-22; each page displayed "Last updated 2026-10-21 UTC".

| Source | Use |
| --- | --- |
| [JSON API status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) | 401 `required` example, `authError` reason and 403 `forbidden` example |
| [IAM permissions for JSON methods](https://docs.cloud.google.com/storage/docs/access-control/iam-json) | Permission per supported route and the overwrite `storage.objects.delete` requirement |
| [IAM roles for Cloud Storage](https://docs.cloud.google.com/storage/docs/access-control/iam-roles) | Pinned 20-role `storage.*` permission snapshot in `src/auth/roles.rs` |

## Public object read source pins

Checked on **2026-10-22** for the
[public object read contract](slices/public-object-reads.md). The SHA-256 values
identify complete HTML or raw source bytes fetched that day, not normalized
extracts or a claim that moving documentation URLs are immutable. Storage pages
displayed a last-update date of 2026-10-21. The original research cutoff above
does not override these later slice-specific checks.

| Source | SHA-256 |
| --- | --- |
| [XML GET Object](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download) | `1a26e94f3a6a2a9c300d81dfca13c0ca0bdc1c671e7e35f5edf9fe36eff7ff43` |
| [XML HEAD Object](https://docs.cloud.google.com/storage/docs/xml-api/head-object) | `d4a2788c37fb4d6e4f703976d32c0a965c7c61418c9d4c96bf8c82dcf610155b` |
| [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `3d254725a572ad08b58bbbc38f32202fc820b361dfb48d9b8b09274bd35f5bdb` |
| [XML status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | `9b786af795be3d89bbc70b11e8e812a204fa5d160e50b9df23b91e795d91801e` |
| [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints) | `217a385a30c18be4925d082dfd4bd3f35eec62d50afd0f4428ac2588b6c0eaf3` |
| [Storage IAM](https://docs.cloud.google.com/storage/docs/access-control/iam) | `4592ab72666799b4788325f200c35c97afcb43a8e930981c5e0579e286667684` |
| [IAM principals](https://docs.cloud.google.com/iam/docs/principals-overview) | `cb2d4042a70f600de7a018c72f6a0a1dcc7eff29ba4fab69415ffa748b7206c5` |
| [Make data public](https://docs.cloud.google.com/storage/docs/access-control/making-data-public) | `863f3184e544e2555d82f9b8369ef513ac9b9bab46030fbaaed227ba6271c39a` |
| [Public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention) | `97eb5fded3996c5bc47d3ef6cf7e831851945b2cd7168db8c441f16312c18372` |
| [Object metadata](https://docs.cloud.google.com/storage/docs/metadata) | `0227a9749462c0c29366d0feab53c73867bbe3cd194c980b49bf60c8fa28aa8c` |
| [JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) | `992c1e95ee04da0709ff187ea743a01801a9bda4a92c6959d4df18e602c67b60` |
| [Go storage/v1.70.0 http_client.go](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/http_client.go) | `a74c0157e327096d641d4a8ca8b544ec6b2e4f8c9e64c463f5d8155e1cec50d0` |
| [Go storage/v1.70.0 reader.go](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/reader.go) | `e0d84a656d4a43610169a9700f12d7aea5ede4b274ca84a5a3e4d06c77a4bfe8` |
| [fake-gcs-server v1.56.1 server.go](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/server.go) | `abe448cd575fbc5687a2527031c3b72fc1ff4b109b12333d1b1d031ab533c005` |
| [fake-gcs-server v1.56.1 object.go](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go) | `cfd884eb03aa429f6b681bef8ca1923c600b24beb850c0620121c0c23110a56b` |

The Go entries identify SDK behavior at the pinned tag. The fake-gcs-server
entries are comparison-only source reviews; neither implementation overrides
Google documentation. No real-cloud differential tests were run.

Independent dependency checks on the same date used the official
[Rust stable channel](https://static.rust-lang.org/dist/channel-rust-stable.toml),
[crates.io sparse registry](https://index.crates.io/config.json),
[Go releases](https://go.dev/dl/?mode=json),
[Storage module](https://proxy.golang.org/cloud.google.com/go/storage/@latest),
[OAuth2 module](https://proxy.golang.org/golang.org/x/oauth2/@latest),
[API module](https://proxy.golang.org/google.golang.org/api/@latest),
[Rust image registry](https://registry-1.docker.io/v2/library/rust/manifests/slim-trixie)
and [Distroless registry](https://gcr.io/v2/distroless/cc-debian13/manifests/nonroot).
All checked stable versions/digests matched the pins; the
[slice-start command output](evidence/public-reads-dependencies.txt) records the
complete detector result, including Actions and auxiliary container inputs.

## Resumable upload source pins

Fetched and read on **2026-10-22** for the
[resumable upload contract](slices/resumable-upload.md). SHA-256 identifies the
complete fetched HTML, JSON or raw source bytes, without normalization. Moving
documentation URLs are dated snapshots; tagged SDK sources and the protobuf
commit below pin upstream identities. The legacy how-to redirects to the
performing guide; its fetched HTML has a distinct digest.

| Source | SHA-256 |
| --- | --- |
| [Perform resumable uploads](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads) | `0973619be28577c2e854b4e9cedb2ac3c90c0d74f6b7a71cee0fcbb6be78e630` |
| [Resumable uploads](https://docs.cloud.google.com/storage/docs/resumable-uploads) | `0e81715be46aaa2352b83e31b45dc18fe5d9b56fded5062c783e315b8f95dc9c` |
| [Legacy JSON resumable how-to](https://docs.cloud.google.com/storage/docs/json_api/v1/how-tos/resumable-upload) | `aac3ebeee22a64cd1e15fd90dfb4468a838eddfb27c1e50aea81ec5960420c16` |
| [JSON objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert) | `4715ca06e4f5478e8c42ae1e376ed5990e4b3a83161b4d27ad610366e0182447` |
| [Live Discovery, revision 20261013](https://storage.googleapis.com/$discovery/rest?version=v1) | `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |
| [JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) | `72a6db7b449430c74671be69460e7c1e8121a8e0f7e74005b3b94790d9f6e856` |
| [Data validation](https://docs.cloud.google.com/storage/docs/data-validation) | `706320d7d4fef670b8edeb2d032b40434937a1e76f830606cc211670fdc01712` |
| [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) | `d6d1fc92e1c62731043d7fccee326765b09c9cf8853bcd07142437ef6c62e4e9` |
| [Retry strategy](https://docs.cloud.google.com/storage/docs/retry-strategy) | `4d1d4073f875961884f8084b2c259dbb6ba9691d4e8286b86db852c8c80d26ca` |
| [JSON IAM permissions](https://docs.cloud.google.com/storage/docs/access-control/iam-json) | `21047651eb43dfbf948c0f13e4b45361b1cc027d7369d801d3c191747f4bbe96` |
| [Storage v2 proto at d73dfb3c3be52245625b951b9c0bcf65163c549f](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto) | `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf` |
| [Go storage/v1.70.0 writer.go](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/writer.go) | `639be4270a948e984c90bb527fd78eb68cf2f2c8b9440bc5faf64d853c38acef` |
| [Go storage/v1.70.0 http_client.go](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/http_client.go) | `a74c0157e327096d641d4a8ca8b544ec6b2e4f8c9e64c463f5d8155e1cec50d0` |
| [API v0.307.0 resumable.go](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/internal/gensupport/resumable.go) | `f02844474c0dd9aef774c2f6605f93b58caa87845f786c37633cdd543fb43141` |
| [API v0.307.0 media.go](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/internal/gensupport/media.go) | `47c81438de78d304c1c2076dddb6a745ac01fe63b6193feee3cb8f8a2bc1003d` |
| [API v0.307.0 storage-gen.go](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/storage/v1/storage-gen.go) | `a21d6476e9c7a20c1853ce8629e63d10572911488fc457e21e182674b6d3222e` |
| [fake-gcs-server v1.56.1 upload.go](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go) | `8713b9787c5cf48b22700f332efc847658e3200d6d74e3fd081b7761783e424c` |

The Go sources establish this pinned client's POST, no-308 override and final
automatic checksum request construction. The fake-gcs-server source is a
comparison only; its alternate x-goog-upload command handling is not an
authoritative JSON specification. Storage v2 is a separate protocol and does
not resolve JSON error ambiguities. No comparison suite or cloud differential
test was run. SPEC-RSM-* entries in `compatibility/gaps.json` retain unresolved
path, error/concurrency, permission timing and automatic multi-origin questions;
SPEC-013 continues to own invocation-token response replay.

## Important scope boundaries

Cache acceleration, geographic redundancy, production durability, Google identity issuance, KMS execution, billing, BigQuery query execution, managed ML enrichment and external network services cannot be certified by a local emulator test. Track their local state/wire simulations separately from external services.

The Cloud Storage API is distinct from Firebase Storage APIs/Security Rules, IAM Credentials signing, Organization Policy administration, Resource Manager tags, Storage Transfer Service, Pub/Sub and Cloud Monitoring/Logging. Their effects and optional adapters belong in the ledger, but this project must not claim to implement those entire services.

Protocol fields and public APIs are a dated baseline. An inventory is not an assurance that every undocumented combination is known. New accepted cases and upstream changes become new pending work rather than silently expanding an implemented claim.

## Reference index

- <https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/>
- <https://doc.rust-lang.org/cargo/commands/cargo-update.html>
- <https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html>
- <https://doc.rust-lang.org/cargo/reference/rust-version.html>
- <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html>
- <https://doc.rust-lang.org/edition-guide/editions/>
- <https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest>
- <https://docs.cloud.google.com/iam/docs/deny-access>
- <https://docs.cloud.google.com/iam/docs/deny-permissions-support>
- <https://docs.cloud.google.com/ruby/docs/reference/google-cloud-storage/latest/Google-Cloud-Storage>
- <https://docs.cloud.google.com/storage-transfer/docs/overview>
- <https://docs.cloud.google.com/storage/docs/access-control>
- <https://docs.cloud.google.com/storage/docs/access-control/iam>
- <https://docs.cloud.google.com/storage/docs/access-control/iam-json>
- <https://docs.cloud.google.com/storage/docs/access-control/iam-roles>
- <https://docs.cloud.google.com/storage/docs/access-control/signed-urls>
- <https://docs.cloud.google.com/storage/docs/access-control/signed-urls-v2>
- <https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually>
- <https://docs.cloud.google.com/storage/docs/authentication>
- <https://docs.cloud.google.com/storage/docs/authentication/canonical-requests>
- <https://docs.cloud.google.com/storage/docs/authentication/signatures>
- <https://docs.cloud.google.com/storage/docs/autoclass>
- <https://docs.cloud.google.com/storage/docs/availability-durability>
- <https://docs.cloud.google.com/storage/docs/aws-simple-migration>
- <https://docs.cloud.google.com/storage/docs/batch>
- <https://docs.cloud.google.com/storage/docs/bucket-lock>
- <https://docs.cloud.google.com/storage/docs/bucket-relocation/overview>
- <https://docs.cloud.google.com/storage/docs/cloud-storage-fuse/overview>
- <https://docs.cloud.google.com/storage/docs/consistency>
- <https://docs.cloud.google.com/storage/docs/cross-origin>
- <https://docs.cloud.google.com/storage/docs/encryption>
- <https://docs.cloud.google.com/storage/docs/encryption/customer-managed-keys>
- <https://docs.cloud.google.com/storage/docs/encryption/customer-supplied-keys>
- <https://docs.cloud.google.com/storage/docs/gsutil_install>
- <https://docs.cloud.google.com/storage/docs/hns-overview>
- <https://docs.cloud.google.com/storage/docs/hosting-static-website>
- <https://docs.cloud.google.com/storage/docs/insights/datasets>
- <https://docs.cloud.google.com/storage/docs/insights/inventory-reports>
- <https://docs.cloud.google.com/storage/docs/insights/reference/rest>
- <https://docs.cloud.google.com/storage/docs/interoperability>
- <https://docs.cloud.google.com/storage/docs/ip-filtering-overview>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/buckets>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/restore>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/create>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/delete>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/get>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/list>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/featureConfigs/patch>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/folders/rename>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/folders.locations/getIntelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/folders.locations/updateIntelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/organizations.locations/getIntelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/organizations.locations/updateIntelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/projects.locations/getIntelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceConfig/projects.locations/updateIntelligenceConfig>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings.revisions/list>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings/get>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings/list>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/intelligenceFindings/projects.locations.intelligenceFindings/summarize>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/bulkRestore>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/move>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/restore>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/rewrite>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/objects/viewFullContext>
- <https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes>
- <https://docs.cloud.google.com/storage/docs/lifecycle>
- <https://docs.cloud.google.com/storage/docs/locational-endpoints>
- <https://docs.cloud.google.com/storage/docs/managed-folders>
- <https://docs.cloud.google.com/storage/docs/metadata>
- <https://docs.cloud.google.com/storage/docs/monitoring>
- <https://docs.cloud.google.com/storage/docs/object-contexts>
- <https://docs.cloud.google.com/storage/docs/object-enrichments/configure-object-enrichments>
- <https://docs.cloud.google.com/storage/docs/object-enrichments/manage-object-enrichments>
- <https://docs.cloud.google.com/storage/docs/object-enrichments/models>
- <https://docs.cloud.google.com/storage/docs/object-holds>
- <https://docs.cloud.google.com/storage/docs/object-lock>
- <https://docs.cloud.google.com/storage/docs/object-versioning>
- <https://docs.cloud.google.com/storage/docs/objects>
- <https://docs.cloud.google.com/storage/docs/org-policy-constraints>
- <https://docs.cloud.google.com/storage/docs/performing-resumable-uploads>
- <https://docs.cloud.google.com/storage/docs/public-access-prevention>
- <https://docs.cloud.google.com/storage/docs/pubsub-notifications>
- <https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket>
- <https://docs.cloud.google.com/storage/docs/rapid/rapid-cache>
- <https://docs.cloud.google.com/storage/docs/rapid/use-objects-in-zonal-buckets>
- <https://docs.cloud.google.com/storage/docs/reference/libraries>
- <https://docs.cloud.google.com/storage/docs/reference/mcp>
- <https://docs.cloud.google.com/storage/docs/reference/rpc/google.longrunning>
- <https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.control.v2>
- <https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2>
- <https://docs.cloud.google.com/storage/docs/reference/rpc/storage-control-operations>
- <https://docs.cloud.google.com/storage/docs/regional-endpoints>
- <https://docs.cloud.google.com/storage/docs/release-notes>
- <https://docs.cloud.google.com/storage/docs/request-endpoints>
- <https://docs.cloud.google.com/storage/docs/request-preconditions>
- <https://docs.cloud.google.com/storage/docs/requester-pays>
- <https://docs.cloud.google.com/storage/docs/resumable-uploads>
- <https://docs.cloud.google.com/storage/docs/samples/storage-s3-sdk-list-objects>
- <https://docs.cloud.google.com/storage/docs/soft-delete>
- <https://docs.cloud.google.com/storage/docs/storage-intelligence/findings/findings-reference-catalog>
- <https://docs.cloud.google.com/storage/docs/storagebatchoperations/reference/rest>
- <https://docs.cloud.google.com/storage/docs/storagebatchoperations/reference/rest/v1/projects.locations.jobs>
- <https://docs.cloud.google.com/storage/docs/tags-and-labels>
- <https://docs.cloud.google.com/storage/docs/uniform-bucket-level-access>
- <https://docs.cloud.google.com/storage/docs/use-object-contexts>
- <https://docs.cloud.google.com/storage/docs/use-tags-for-soft-delete>
- <https://docs.cloud.google.com/storage/docs/xml-api/get-bucket-metadata>
- <https://docs.cloud.google.com/storage/docs/xml-api/get-hmac-list>
- <https://docs.cloud.google.com/storage/docs/xml-api/get-object-encryption>
- <https://docs.cloud.google.com/storage/docs/xml-api/get-object-retention>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-bucket>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-hmac-create>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-hmac-delete>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-hmac-update>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-object-complete>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-object-forms>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-object-multipart>
- <https://docs.cloud.google.com/storage/docs/xml-api/post-object-resumable>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-create>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-object-compose>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-object-copy>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-object-multipart>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-object-retention>
- <https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload>
- <https://docs.cloud.google.com/storage/docs/xml-api/reference-methods>
- <https://docs.cloud.google.com/storage/docs/xml-api/reference-status>
- <https://docs.cloud.google.com/storage/quotas>
- <https://docs.cloud.google.com/vpc-service-controls/docs/supported-products>
- <https://docs.docker.com/build/building/best-practices/>
- <https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference>
- <https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html>
- <https://firebase.google.com/docs/emulator-suite/connect_storage>
- <https://github.com/fsouza/fake-gcs-server/blob/main/README.md>
- <https://github.com/fsouza/fake-gcs-server/blob/main/fakestorage/server.go>
- <https://github.com/googleapis/google-api-python-client/blob/main/googleapiclient/discovery_cache/documents/storage.v1.json>
- <https://github.com/googleapis/google-cloud-rust>
- <https://github.com/googleapis/googleapis/blob/master/google/cloud/location/locations.proto>
- <https://github.com/googleapis/googleapis/blob/master/google/cloud/storagebatchoperations/v1/storage_batch_operations.proto>
- <https://github.com/googleapis/googleapis/blob/master/google/cloud/storagebatchoperations/v1/storagebatchoperations_v1.yaml>
- <https://github.com/googleapis/googleapis/blob/master/google/cloud/storageinsights/v1/storageinsights.proto>
- <https://github.com/googleapis/googleapis/blob/master/google/cloud/storageinsights/v1/storageinsights_v1.yaml>
- <https://github.com/googleapis/googleapis/blob/master/google/storage/control/v2/storage_control.proto>
- <https://github.com/googleapis/googleapis/blob/master/google/storage/v2/storage.proto>
- <https://github.com/googleapis/nodejs-storage/blob/main/src/storage.ts>
- <https://github.com/googleapis/python-storage/blob/main/google/cloud/storage/_helpers.py>
- <https://github.com/googleapis/storage-testbench>
- <https://guides.rubyonrails.org/active_storage_overview.html>
- <https://rust-lang.github.io/rustup/overrides.html>
- <https://storage.googleapis.com/$discovery/rest?version=v1>
- <https://tokio.rs/tokio/tutorial/channels>
- <https://www.sqlite.org/wal.html>
