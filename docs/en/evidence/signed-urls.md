# Integrated V4 RSA signed GET/HEAD/PUT verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked **2026-10-23**. Code commits: `86744f4c3b5530771c5c4547319e0d4d2b945431`
and `8e1831e8a929fd034f70ade65093aeb86e33d7cc`.
This is the final combined evidence set for the
[download](../slices/signed-download.md), [bounded XML PUT](../slices/xml-put-object.md)
and [signed-upload](../slices/signed-upload.md) contracts. Their original per-slice
evidence remains historical; this integrated result supersedes earlier next-slice
and optional canonical payload statements.

## Checks

| Command | Result | Evidence |
| --- | --- | --- |
| `just check` | PASS, 626 Rust tests; zero failed/ignored, formatting, Clippy, ledger and shell checks | [Rust checks](signed-urls-checks.txt) |
| `GCS_MIMIC_SDK_SCHEME=https just check-sdk` | PASS, Go vet and all 44 top-level tests, Ruby origin guard and both auth profiles including signed GET/PUT | [SDK output](signed-urls-sdk.txt) |
| `cargo xtask dependencies check` | PASS, all 36 inputs CURRENT | [Freshness](signed-urls-dependencies.txt) |
| `actionlint` | PASS | Existing Go CI remains Go-only; `just check-sdk` explicitly enables Ruby |
| `just check-containers` | PASS, linux/amd64 emulated and linux/arm64 native | [Containers](signed-urls-containers.txt) |
| Native/source-image/strict-Compose signed guide | PASS, generated key/public-only server registry, helper GET/PUT and failures, restart and separate peer signature | [Guide transcript](signed-urls-guide.txt) |
| Markdown lint and generated English/Japanese matrices | PASS | [Documentation checks](signed-urls-docs.txt) |

The source-built final image IDs are
`sha256:52212c51fabd4d34e5b489b798efdd29b5b32412d7f77d5ffe4eaeb38a92da9d`
for linux/amd64 and
`sha256:3dce28ff80e9512543b767d81aba0f2219b121f2d88caf78a3be16f213fe4cc3`
for linux/arm64. The guide reused that amd64 image, with no extra container build.
Its disposable Compose project, volume and key directory were removed. Dynamic
published ports are inspected again after restart and URLs are freshly signed;
an existing signature's host is never rewritten. Native startup removes
Compose-only image/port environment variables.

The development guide disables only MD013 to match the repository's existing long
prose/table style; all other Markdown checks remain enabled. No release, push,
registry publication or billable cloud operation was performed.
Raw Docker/guide transcripts preserve Compose's trailing spaces; whitespace
validation excludes those two raw files and passes for source and documentation.

## Verified behavior and limits

Nine signature unit tests pin canonical strings, clock boundaries, parser/key
validation and final Host/path/query/header binding. The 72-test auth HTTP suite
includes existing public/IAM/CORS behavior, all 13 XML PUT wire tests and four new
signed PUT cases. Those cover private exact bytes/metadata, signed
Content-Type/Content-MD5/custom metadata/conditions, required extension-header
rejection, standard unsigned extras, pre-body signer/expiry/tamper failure,
signed/unsigned streamed SHA256, failed-overwrite preservation, restart, concurrent
generation-match publication and signed XML PUT preflight/actual CORS responses.
Existing shared storage tests retain transactional create/delete/condition and
revocation/recreated-bucket checks. No schema or buffering limit changes.

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

DESIGN-029/030/032/034/036/038/045 are IMPLEMENTED for their scoped contracts.
DESIGN-033/035/037/044/055, UX-006, DESIGN-002 and EXT-XML-012/017/022 retain
explicit pending cases: virtual-host addressing, response overrides/duplicate
parity, optional digest provider/error semantics, exact signature errors,
separately configured/proxy-derived origins, PAP and advanced XML/IAM behavior.
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
