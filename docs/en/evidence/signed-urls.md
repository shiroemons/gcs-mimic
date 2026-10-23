# Integrated signed access, virtual hosts and public access prevention

<!-- markdownlint-configure-file {"MD013": false} -->

Checked **2026-10-23**. Production integration: `54ad326`; integrated interaction
tests: `d9541d5` and `822f39c`. This is the single combined evidence set for the
[download](../slices/signed-download.md), [bounded XML PUT](../slices/xml-put-object.md)
and [signed-upload](../slices/signed-upload.md) contracts, with
[response overrides](xml-response-overrides.md), [virtual hosts](xml-virtual-host.md)
and [PAP](public-access-prevention.md). Linked feature evidence preserves the
original acceptance and official source pins; the raw logs below cover the
integrated result. Earlier path-style-only, override/PAP-pending and optional
canonical payload statements are superseded for the declared delivered scopes.

## Checks

| Command | Result | Evidence |
| --- | --- | --- |
| `cargo test --locked --offline --test auth_http virtual_host_` | PASS, eight virtual-host wire tests including three integration/CLI cases | [Rust checks](signed-urls-checks.txt) |
| `just check` | PASS, 656 Rust tests; zero failed/ignored, fmt, Clippy, ledger and shell checks | [Rust checks](signed-urls-checks.txt) |
| `GCS_MIMIC_SDK_SCHEME=https just check-sdk` with supplied wildcard-SAN certificate and CA | PASS, Go vet and all 46 top-level tests, Ruby origin guard and both auth profiles including signed GET/PUT and response overrides | [SDK output](signed-urls-sdk.txt) |
| `cargo xtask dependencies check` | PASS, all 36 inputs CURRENT | [Freshness](signed-urls-dependencies.txt) |
| `actionlint` | PASS | Existing Go CI remains Go-only; `just check-sdk` explicitly enables Ruby |
| `just check-containers`, then affected architecture checks after Docker recovery | PASS, complete linux/amd64 emulated and linux/arm64 native smoke/seed gates; AMD64 image reused | [Containers](signed-urls-containers.txt) |
| Updated signed/PAP/virtual-host guide | PASS natively and from the start with source-image/strict Compose: private GET/PUT/failures, overrides, virtual hosts, PAP, public reactivation, enforced-PAP restart and separately signed peer access | [Guide transcript](signed-urls-guide.txt) |
| Markdown lint and generated English/Japanese matrices | PASS, nine updated Markdown files, both matrices and 591-row ledger validate/check | [Documentation checks](signed-urls-docs.txt) |

The final image IDs are
`sha256:ba6b0f654ce95d6f35a4e559246ede750e434c0a2bc5f528f9e8d3a4d06203af`
(linux/amd64) and
`sha256:974cb88213cdd2f4162231acf1e942393544881a467781a11dd578bd0b5034ea`
(linux/arm64). The initial full container gate was interrupted by an unresponsive
Docker daemon. After the user restarted Docker Desktop, both architecture gates
ran from the start and exited zero: AMD64 used
`bash scripts/smoke-containers.sh --image gcs-mimic:bucket-smoke-amd64-90206 linux/amd64`
without rebuilding, and ARM64 used `bash scripts/smoke-containers.sh linux/arm64`,
resuming its interrupted build cache. The guide reused that same AMD64 image and
its final strict-Compose run also started from the beginning and exited zero.
The final source then passed `just check` again. No daemon restart, volume repair
or global image/volume pruning was performed by this integration worker.
Only its disposable projects, containers, volumes and key directories were removed.
The historical path-style guide remains labeled separately in the same log.
Dynamic published ports are inspected after restart and URLs freshly signed;
an existing signature's host is never rewritten.
Native startup removes Compose-only image/port environment variables.
SDK verification uses Ruby 4.0.9, Bundler 4.0.23 and the existing pinned gem cache.
Its supplied server certificate has `localhost`, `127.0.0.1` and
`*.storage.example.invalid` SANs, signed by an isolated development CA.
CA, hostname and SNI verification remain enabled; no system trust is changed.

The development guide disables only MD013 to match the repository's existing long
prose/table style. Matrix lint additionally uses MD024 `siblings_only` because
feature-kind sections repeat category headings; other checks remain enabled. No release, push,
registry publication or billable cloud operation was performed.
Raw Docker/guide transcripts preserve Compose's trailing spaces; whitespace
validation excludes those two raw files and passes for source and documentation.

## Verified behavior and limits

Nine signature unit tests pin canonical strings, clock boundaries, parser/key
validation and final Host/path/query/header binding. The integrated auth HTTP
suite retains existing public/IAM/CORS behavior, all 13 XML PUT wire tests and
signed PUT cases. Those cover private exact bytes/metadata, signed
Content-Type/Content-MD5/custom metadata/conditions, required extension-header
rejection, standard unsigned extras, pre-body signer/expiry/tamper failure,
signed/unsigned streamed SHA256, failed-overwrite preservation, restart, concurrent
generation-match publication and signed XML PUT preflight/actual CORS responses.
Existing shared storage tests retain transactional create/delete/condition and
revocation/recreated-bucket checks. PAP adds schema 13 configured state and
transactional migration, without changing body buffering limits.

`virtual_host_signed_response_overrides_bind_query_and_preserve_metadata`
checks both override headers on signed virtual GET/HEAD, unchanged generation,
metadata/length and query tamper failure without override leakage into errors.
`virtual_host_cli_signs_literal_response_queries_for_the_final_bucket_host`
invokes the actual CLI with `--style virtual-hosted` and both literal `--query`
options, then requests its unchanged signed GET/HEAD URLs.
`virtual_host_pap_preserves_signed_transfers_and_reactivates_stored_public_grants`
covers both public principal sets, path/virtual public baselines before PAP,
anonymous/registered/public-only signer denials under PAP, explicit signer
GET/PUT, signed response override tampering, unchanged object metadata and
policy/etag, and reactivation after disabling PAP. These supplement the full
[virtual-host](xml-virtual-host.md), [override](xml-response-overrides.md) and
[PAP](public-access-prevention.md) acceptance sets.

The native guide executes the exact PAP signed GET/PUT block from the slice,
adds a real `allUsers` policy before enforcement, compares the stored policy
before/during/after PAP and verifies anonymous reactivation on both endpoint
styles. Its curl `--resolve` maps only the connection address; Host and signed
URLs remain unchanged. The guide documents opt-in configuration, DNS and HTTPS
SAN requirements. The Compose version uses public-only server key mounts and
a network-disabled signing container.

The helper signs query canonical payload as UNSIGNED-PAYLOAD. A supplied SHA256
is checked incrementally before the upload End marker. Invalid signatures and
permissions fail before body polling; digest failures never publish. Existing
post-EOF commit cancellation and durability limits remain unchanged. No new
large-transfer RSS/cgroup, actual-browser, Rails or real Google execution is claimed.

Go 1.27.3 / storage v1.70.0 / API v0.308.0 tests unmodified official SignedURL GET/PUT
with a signed ContentType through a default-port authority mapped by a guarded
local dialer. The SDK drops a non-default port from canonical Host; that unchanged
URL is explicitly rejected. Independent explicit-port download correction remains
separate evidence. The digest-bound SDK PUT substitutes its SHA256 into the payload
line (`storage.go:871-883`), contrary to the documented query rule. Matching and
changed bodies both fail 403 SignatureDoesNotMatch and preserve the previous
resource. Omit SDK SHA256 signing options or use the local helper; see
SPEC-GO-SIGNER-PORT and SPEC-SIGNED-PAYLOAD.

Ruby 4.0.9 / Bundler 4.0.23 / storage 1.62.1 uses official File#signed_url GET/PUT.
Independent RSA verification proves that default custom-endpoint signing binds
storage.googleapis.com instead of the emitted endpoint Host. The normal URL fails
403 SignatureDoesNotMatch. The supported bucket_bound_hostname option preserves
the actual host:port, with an explicit bucket/name File path compensating for the
SDK's omitted bucket prefix before signing. Generated URLs then pass unchanged;
query/object/Host/method/header tampering and anonymous reads fail. Origin guards,
disabled proxies/redirects and verified CA/SAN trust confine requests to the emulator.
See SPEC-RUBY-SIGNER-ENDPOINT and the [Ruby harness](../../../sdk-tests/ruby/README.md).

DESIGN-029/030/032/033/034/036/038/045/048 and DESIGN-023/025 are IMPLEMENTED
for their scoped contracts. BEHAVIOR-BUCKET-014 and DESIGN-022 are IMPLEMENTED
with LOCAL_SIMULATION fidelity for parent enforcement. DESIGN-035 stays PARTIAL
for exact duplicate/ambiguous query behavior (SPEC-SIGNING-EDGES), with local
override validation and anonymous/HEAD extensions in SPEC-XML-RESPONSE-OVERRIDES.
DESIGN-037 stays PARTIAL under SPEC-SIGNED-PAYLOAD; DESIGN-044 retains exact
signature errors/precedence under SPEC-SIGNING-EDGES. DESIGN-055 retains
separately configured/proxy-derived origins; DESIGN-002 and broad XML/IAM rows
retain their named advanced and provider cases. DESIGN-024 remains PARTIAL for
ACL/default-object-ACL exceptions and SPEC-PAP-EDGES. UX-006 is IMPLEMENTED after
the complete current native/source-image/strict-Compose guide passed, including PAP.
HMAC, Authorization-header V4, V2, POST policy and signed resumable rows are unchanged.
Next: delivery order 18, actual signed browser transfers using existing CORS/TLS.

## Source pins

Official documentation fetched on 2026-10-23. Digests identify complete fetched
HTML bytes; prose is not vendored. Existing Discovery/protobuf identities remain
in [the source manifest](../../../compatibility/upstream/source-manifest.json).

| Source | SHA256 |
| --- | --- |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `11b87e9e96e2eafc5dad9dde279aab29dd4a10e3cb0ac9b02232a39cf793cd4f` |
| [Signatures](https://docs.cloud.google.com/storage/docs/authentication/signatures) | `afde1bae2d0b87d7ccc9be1beaab6d7f06b6d96ba265a3dee9f906f13836af43` |
| [Manual signing](https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually) | `bd4494c37cfbb0b30f42914eec0ea500eb1dbcde37ba71e3e7713c27639d7ce2` |
| [Signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls) | `75aec2a81c50d295484f742407f40e7e757d15585849b9bb9424e9e022ad3c41` |
| [XML PUT](https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload) | `4479bb771b53cbee9f798785a7130733d6b7c5049d957bb4fb860d9cd15ab45e` |
| [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `fb26611a9c098ef17d528bed63d588560b782737f2cdf165aac0e6245a376ecb` |
| [XML errors](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | `8fe342a98faecda1af86e14e1c110d5e0f9cb433de1450fd0580eb0a3be25316` |
| [Go storage.go, storage/v1.70.0](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/storage.go) | `fb15593b2ab6448ec3a622ad4ae91ad2972a9bb0e14ec34792c87d91bbb74a7d` |
| [Ruby signer_v4.rb, 1.62.1](https://github.com/googleapis/google-cloud-ruby/blob/google-cloud-storage/v1.62.1/google-cloud-storage/lib/google/cloud/storage/file/signer_v4.rb) | `392dea55133add91c6717de70b47f572287d34ddd8be37d199a9cb256fc596fb` |

The application Cargo.lock SHA256 is
`3b8100d00d9d1b534fce001b07e8329cd2d26aaba19bdfe4678dd79ae718116b`;
Go go.sum is `1fe4e981f7239be32bec257908d18fea019b970e85ec86ffdcf559094d01fc38`;
Ruby Gemfile.lock is `ff1b0325b28745729a962668ffbb906167c895e19104b5ea393e509ba46c3df6`.
No dependency version was added or changed by the upload slice.

## Pinned comparison source review

Checked **2026-10-23**, source review only. No new reference-emulator execution
or Google request is claimed. README comparisons use these fixed source files:

| Source | SHA256 |
| --- | --- |
| [fake-gcs-server object handlers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go) | `cfd884eb03aa429f6b681bef8ca1923c600b24beb850c0620121c0c23110a56b` |
| [fake-gcs-server router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go) | `abe448cd575fbc5687a2527031c3b72fc1ff4b109b12333d1b1d031ab533c005` |
| [gcp-storage-emulator object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) | `e382f1715e55805a0b7459c1f9c8f21b8f38443da6c8d863ea6994bf1c1f4c4f` |
| [gcp-storage-emulator router](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py) | `b0227517548e0c34304b06029712351de277bd2a8bb1c7c45e71d0a27ec55837` |

The fake-gcs-server download handler sets both response override headers, and its
router includes bucket-host reads/uploads. The gcp-storage-emulator handler sets
Content-Disposition only; public routes are path-style without HEAD or bucket
Host selection. Neither reviewed request path verifies signing credentials or
public IAM/PAP grants. Accepting signed-shaped requests is distinguished from
signature/expiry/permission enforcement.
