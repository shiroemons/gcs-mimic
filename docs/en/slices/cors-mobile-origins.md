# CORS origins for browsers and iOS applications

<!-- markdownlint-configure-file {"MD013": false} -->

Official sources checked on **2026-10-23**. This research supplements the
[CORS contract](cors.md) for DESIGN-051, DESIGN-052, DESIGN-053 and UX-007.
The feature ledger and execution evidence own implementation claims. Source
inspection does not establish real Google Cloud or iOS device compatibility.

## What Google documents

The [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets)
defines `cors` as ordered rules containing string arrays `origin`, `method`,
`responseHeader` and integer `maxAgeSeconds`. Origins refer to the web origin
concept; neither this resource nor the checked Discovery schema declares an
HTTP/HTTPS-only origin enum. This absence does not prove that Google accepts
every arbitrary string or custom scheme. Discovery revision `20261013` and its
digest are pinned in the [shared contract](cors.md#official-inputs).

The [setup guide](https://docs.cloud.google.com/storage/docs/using-cors) documents
configuration through the console, CLI, client libraries and JSON/XML APIs.
Reading and changing rules require `storage.buckets.get` and
`storage.buckets.update`, respectively. Multiple rules are allowed, and JSON
`{"cors": []}` removes all rules. A CLI CORS file contains the rule array without
the outer `cors` property. These are Google configuration interfaces; the
emulator's supported configuration interface is its documented JSON API.

[Google's CORS overview](https://docs.cloud.google.com/storage/docs/cross-origin)
separates JSON defaults from bucket-configured XML behavior. JSON reflects the
request origin independently of bucket rules; XML evaluates those rules.
XML never grants CORS credential mode with Allow-Credentials. Specifying an
`Authorization` request header and using browser `credentials: "include"` are
separate choices. A successful preflight never grants object permission.

## Documented origins and unresolved provider behavior

| Input or behavior | Official basis | Interpretation for this project |
| --- | --- | --- |
| `http://app.example` and `https://app.example` | Google's examples and troubleshooting | Distinct scheme/host/port origins; a configuration URL is an origin, without an object path. |
| HTTP omitted port and `:80`; HTTPS omitted port and `:443` | Explicit troubleshooting examples | Equivalent within the same scheme. Other ports remain distinct. |
| `http://localhost:8080` | Explicit troubleshooting example | Does not match another port or `localhost.example.com`. |
| Whole-origin `*` | Bucket resource and overview | Matches any origin; does not grant authorization or XML credential mode. |
| `https://*.example.com` | Current CORS configuration examples | Subdomain wildcard is documented. Arbitrary regexes, wildcard schemes and arbitrary glob positions are not established by these examples. |
| `capacitor://localhost`, `ionic://localhost`, other custom tuple origins | Schema uses strings and refers to origin standards, without a scheme enum | Preserve and evaluate supported serialized origins locally; Google acceptance and device emission remain unverified. |
| Literal `null` | Origin serialization standards | Represents an opaque origin, rather than a unique app identity. Local handling does not establish Google's acceptance of a `null` bucket rule. |
| Raw `file:///...`, `data:...`, `blob:...`, or an app deep-link URL | These are resource URLs rather than necessarily serialized origins | Do not derive an allowlist by copying the document URL; inspect the actual outgoing Origin. |

The port and hostname cases come from
[Google troubleshooting](https://docs.cloud.google.com/storage/docs/troubleshooting#cors).
The wildcard examples come from
[Google CORS configurations](https://docs.cloud.google.com/storage/docs/cors-configurations).
Google's guides do not document a complete acceptance matrix for custom mobile
schemes, opaque origins, IDNA variants or malformed origin strings. No real-cloud
configuration experiment was authorized or performed to resolve those edges.

[RFC 6454, sections 4-7](https://www.rfc-editor.org/rfc/rfc6454) describes
scheme/host/port comparison, lowercase scheme/host construction and serialization.
Unsupported schemes can yield opaque origins serialized as `null`; `file`
handling can be implementation-defined. The RFC's syntax therefore does not
guarantee that a WebView emits a custom scheme as a tuple origin. In particular,
several unrelated opaque documents can serialize to the same `null` value.

## Separate the three URLs in an iOS application

Apple's [custom app URL scheme guide](https://developer.apple.com/documentation/xcode/defining-a-custom-url-scheme-for-your-app)
describes registering links that launch an app and deliver navigation data.
Registering `myapp:` does not set a WebView's web security origin and does not
turn a Storage API request into a `myapp:` transport request.

Apple's [WKURLSchemeHandler](https://developer.apple.com/documentation/webkit/wkurlschemehandler)
loads WebView resources for schemes WebKit does not handle.
[WKSecurityOrigin](https://developer.apple.com/documentation/webkit/wksecurityorigin)
separately represents the resource's protocol, hostname and port. These APIs
describe custom resource loading and origin identity; they do not promise a
particular outgoing Origin header for every custom scheme and iOS version.

For a hybrid app, distinguish:

1. The native deep link, such as `myapp:album?index=1`, used to open the app.
2. The loaded WebView document and its actual serialized Origin, potentially
   `capacitor://localhost`, another configured value, or an opaque origin.
3. The request destination, such as `https://dev-storage.example/bucket/object`.
   The emulator's listener, public/API endpoints and TLS certificate must match
   this reachable destination. A custom source origin does not add a custom
   server transport or change an advertised resumable URL.

The [Capacitor v8 configuration reference](https://capacitorjs.com/docs/config)
sets `server.hostname` to `localhost` and `server.iosScheme` to `capacitor` by
default. It identifies `ionic` as a migration-related scheme. It also states
that iOS cannot configure schemes WKWebView already handles, including HTTP
and HTTPS, through `iosScheme`. These settings describe the app's local content;
they do not configure Google bucket permissions or guarantee a network Origin.

[CapacitorHttp](https://capacitorjs.com/docs/apis/http) can use native networking
and can optionally replace `fetch`/XHR; replacement is disabled by default.
A successful native bridge request therefore does not establish that an
unpatched WebView browser request passes CORS. Record which path the app uses.

## SDK configuration is separate from browser enforcement

The official [Go CORS type](https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest#CORS)
uses origin/method/header string slices and a duration. The official
[Ruby CORS helper](https://docs.cloud.google.com/ruby/docs/reference/google-cloud-storage/latest/Google-Cloud-Storage-Bucket-Cors)
accepts strings or arrays. Both document origin and method `*`; neither reference
adds a mobile scheme allowlist. Their serialization is not evidence that Google
will accept or serve a particular custom origin.

Ruby Storage 1.62.1's `Cors::Rule` supplies `1800` seconds when `max_age` is omitted;
its published helper reference documents the same default. This client-supplied
value differs from the HTTP service's documented `3600` default when the wire
field is absent. Set an explicit age when comparing clients and wire tests.

## Verification boundary

Local wire tests can exercise a supplied `Origin: capacitor://localhost` or
`Origin: ionic://localhost`, JSON reflection and independent XML rule matching.
They can also prove denial after changing the scheme, host, port or rule, and
check HTTP/HTTPS default-port equivalence. Such a test supplies the header itself;
it does not prove that Apple WebKit generates that header.

The [browser evidence](../evidence/cors-browser.md) concerns the explicitly
recorded desktop browser and HTTPS origins. No iOS device, iOS Simulator,
Capacitor application or native networking bridge was executed for this slice.
No real Google project/bucket was contacted. A future mobile integration check
must record the iOS/WebKit and framework versions, effective document URL,
observed Origin, native-versus-WebView networking path, destination/TLS trust,
preflight and actual response, and independent object authorization result.

Apple's checked Markdown source snapshots have SHA-256 values
`c301772989b580c7e18a81c11c1b9cd22478433d921f73c45228e5a82c4b8d35`
for `WKURLSchemeHandler`,
`e4c69027da0326a6221fa8356afa12985df0b76021e20d438247e3ae2a67d021`
for `WKSecurityOrigin`, and
`ba5b47c72eafb4c6eddbffc31231b85126ea2680463bfe22236342736347481a`
for the custom app URL scheme guide. Their accessible source URLs are the linked
Apple documentation URLs with `.md` appended. These source identities are not
device execution evidence.
