# HTTP adapter boundaries

Management routes, including sync controls, refuse every Origin header and every
Sec-Fetch-Site value other than `none` with 403 before route-specific gates.
Tools that send neither header keep working, including multipart fixtures and
the empty-body delete_all alias. A public DNS Host receives 421 unless it is a
configured API/public host, virtual-host base domain or `admin_allowed_hosts`
entry. IP literals, single-label service names and reserved local DNS suffixes
are allowed independently of ports. Host matching is case-insensitive and strips
one trailing dot only for this guard; XML routing keeps its separate contract.
Duplicate Host headers and invalid authorities receive 400. This protects
against browser CSRF and DNS rebinding without changing Google API permissions.

`admin_allowed_hosts` accepts up to 32 comma-separated DNS names. Configure it
through `--admin-allowed-hosts`, `GCS_MIMIC_ADMIN_ALLOWED_HOSTS` or flat JSON.
Kubernetes short names such as `gcs-mimic.my-namespace` and
`gcs-mimic.my-namespace.svc`, or names under a custom cluster domain, need explicit
entries, for example `--admin-allowed-hosts gcs-mimic.my-namespace.svc`.

Unknown shared-listener routes return a JSON `404 notFound` envelope with
`error.code`, `error.message` and `error.errors` entries. Invalid XML bucket
paths retain this JSON boundary. Health and readiness probes retain their local
status documents. A zero Content-Length, including `00`, denotes no body;
Transfer-Encoding and malformed nonzero lengths are treated conservatively.
JSON media 304 construction tolerates absent optional representation headers.

Invalid object page tokens return a fixed error message without echoing the
client's token. CORS lookup diagnostics retain the existing backend allow-list;
I/O causes expose only ErrorKind, never the underlying filename or message.

Body collection uses amortized buffer growth within the existing caps and
preallocates an exact bounded size hint. Its generic Buf loop remains necessary
for noncontiguous body data. Glob matching reuses one traversal stack for the
whole name. Hexadecimal decoding uses the shared codec. Origin syntax checks,
resumable query sorting, legacy bucket response conversion and connection timers
avoid repeated allocations without changing their declared protocol behavior.
No comparative throughput or memory improvement was measured.

## Verification

Not run: SDK suites, container builds and provider differential tests.
