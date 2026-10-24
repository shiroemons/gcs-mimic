# Configuration Host matching evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Executed on **2026-10-24** with the existing Go **1.27.3 darwin/arm64** toolchain.
The comparison target is fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`, source archive SHA-256
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.
No compiler/dependency installation was needed; modules were already cached and
the build used `-mod=readonly`. Build/server/fixture writes stayed in temporary
directories. Each public-authority server has its own log/data target and is
stopped after its probes, with the final server retained only for raw Host errors.

The [comparison script](../../../scripts/compare-reference-emulator.sh) builds
the pinned native binary, starts it under an empty environment on loopback, and
uses seeded object bodies of the form `bucket=NAME;object=NAME` to identify the
selected resource. Seed directories avoid making API bucket-name acceptance an
assumption of the routing probe. Case-only directory names are excluded so the
fixtures remain distinct on case-insensitive filesystems.

```sh
# Replace this placeholder with the root of an installed Go 1.27.3 distribution.
reference_go_root='<installed-go-1.27.3-root>'
env -u GOPROXY -u GOSUMDB \
  GOROOT="$reference_go_root" PATH="$reference_go_root/bin:$PATH" \
  GCS_MIMIC_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server \
  GCS_MIMIC_REFERENCE_ONLY=1 bash scripts/compare-reference-emulator.sh \
  > "$TMPDIR/configuration-reference.log"
awk '/^(pinned reference |archive SHA256 |go version |checkout source commit |comparison script blob |HOST |HOST-ASSERT )/' \
  "$TMPDIR/configuration-reference.log" > docs/en/evidence/configuration-host-matching.txt
```

Set both PATH and GOROOT: an inherited version-manager GOROOT can otherwise mix
compiler/standard-library versions, and an older PATH Go may attempt a toolchain
download. The command retains normal Go checksum verification and permits module
downloads from the locked upstream module graph when the cache is cold. Offline
builds may add `GOPROXY=off` only after caching that graph and the exact pinned
toolchain; disabling the checksum database is not required. The script validates
the source digest, uses the repository's pinned toolchain, and shuts down all child
servers. Source retrieval (and uncached Go modules) are the only external reads;
no Google service, metadata
server, publisher, container or real bucket is contacted. The memory backend is
used for Host routing; the script's existing filesystem/memory bucket comparison
also executes. HTTP requests use HTTP/1.1, no proxy, and an explicit Host header.
Absolute-form requests use curl's explicit request target; netcat sends HEAD and
duplicate, malformed and missing Host requests verbatim. HEAD records extract
the actual bytes after the header terminator, so the empty-body assertion does
not depend on curl discarding a response body.

The [raw records](configuration-host-matching.txt) retain all **189** Host
requests, status/selected content headers and exact response bodies (or full raw
response for transport errors). They also retain the pinned source/toolchain
header, checkout source commit, exact comparison-script Git blob ID and final
assertion result; the extraction command above omits only unrelated bucket probes
and comparison prose. The checkout commit identifies the base revision; the blob
identifies the executed script exactly even when the working tree is modified
(`git hash-object scripts/compare-reference-emulator.sh` reproduces it).
No path redaction or response normalization was required. Public authorities are
`storage.example.invalid`, `storage.example.invalid:80`,
`storage.example.invalid:8443`, `127.0.0.1`, `[::1]`, and `[::1]:8443`.

| Probe | Observed selection/result |
| --- | --- |
| Public authority without a port | DNS with absent/80/443/8443 request ports and IPv4 with absent/8443 request ports select the path bucket; 200 marker confirms selection |
| Explicit public port 80 or 8443 | Only that exact authority selects the path bucket; absent/differing ports select the fallback hostname bucket |
| Bracketed IPv6 | `[::1]` and `[::1]:8443` each require exact authority; a mismatch returns 404 in these fixtures |
| DNS case/trailing dot/lookalike | No case folding or dot trimming in public matching; lowercase dot/lookalike fixtures return 200 from their entire-hostname buckets, while case-varied requests return 404 |
| Simple virtual bucket | `probe-bucket.PUBLIC_HOST` selects `probe-bucket`; its port rules mirror the public authority's omitted/explicit port |
| Dotted/overlapping virtual prefix | `dotted.probe-bucket.storage.example.invalid` and `probe-bucket.deep.storage.example.invalid` select the entire-hostname fixture, not a dotted bucket under the public suffix |
| JSON reserved route on bucket host | `/storage/v1/b?project=compare-project` returns a JSON bucket list even though an object exists at that path in the virtual bucket |
| Public and virtual bucket roots | GET returns XML lists; arbitrary-host root returns 404 `404 page not found` |
| Arbitrary Host | `fallback.invalid/object` selects bucket `fallback.invalid`, object `object`, with a 200 marker |
| Unknown path bucket/object | 404 `Not Found`; HEAD of an existing object is bodyless 200 |
| PUT followed by GET | Six 200 uploads and six exact `host-put` reads. The synthetic `X-Goog-Algorithm=probe` selects upstream's signed-upload branch without real credentials; this proves routing, not signature validation |
| Conflicting absolute-form target/Host | Target authority wins: a public target selects the path bucket despite Host=`fallback.invalid`; reversing them selects the fallback bucket |
| Duplicate/malformed/missing HTTP/1.1 Host | All three receive transport 400 before routing, with raw responses retained |

The virtual DNS case-sensitivity claim is a source inference: the mixed-case
probe is 404 whether matching is sensitive or not because the selected
mixed-case bucket is absent; case-only fixtures cannot be distinguished on a
case-insensitive filesystem. Likewise, public virtual routes are registered
before public path-style routes in the pinned source, but the probes cannot
match both routes at once. Neither detail is claimed as a measured distinction.

Final assertion output:

```text
HOST-ASSERT all 189 records: authority/ports/IPv6, virtual/fallback/DNS, JSON, absolute targets, roots, HEAD, errors and PUT: PASS
```

Assertions cover every recorded case, expected case counts, all 66 authority
combinations and selected fixture bodies. Both absolute-target directions,
explicit-port mismatches, exact IPv6 matches, single/dotted fallback, trailing
dots, artificial IP virtual hosts, XML root lists and raw HEAD bodies are checked.
The final run's 189 status/selected-bucket/body projections match the previous
recording; volatile JSON/XML timestamps, generations, origins and transport dates
are excluded from that comparison, not rewritten in the raw evidence.

These observations resolve the contract's reference investigation. They do not
pass gcs-mimic's pending implementation cases. The
[normative contract](../slices/configuration-workflows.md#host-matching-measured-reference-and-normative-rules)
preserves local signature/authority rejection, supported methods/structured XML
errors, IP virtual-host disabling, default-port normalization and explicit domain
extensions. No full upstream suite, real signing/authentication, TLS, dual
listener, provider parity or performance claim follows from these probes.
