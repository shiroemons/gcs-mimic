# Strict local authentication and public URL helper

Contract checked: **2026-10-22**. Scope: DESIGN-001, DESIGN-003, DESIGN-004,
DESIGN-008 and DESIGN-046, with contributing cases for DESIGN-002, DESIGN-082 and the
JSON bucket/object method rows JSON-BUCKETS-001/002/005/006 and
JSON-OBJECTS-004/005/007/008/010/015.

This is the first part of roadmap item 5. It adds explicit authentication profiles,
a local principal registry, project-level predefined-role enforcement on the
existing JSON routes and a pure public URL builder. Bucket/object IAM policies,
`allUsers`/`allAuthenticatedUsers` grants, ACLs, XML object endpoints and signed URLs
are the next slice.

## Sources

All pages were checked on 2026-10-22; each displayed "Last updated 2026-10-21 UTC".

- [JSON API status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes):
  the 401 `required` example, the `authError` reason and the 403 `forbidden` example.
- [IAM permissions for JSON methods](https://docs.cloud.google.com/storage/docs/access-control/iam-json):
  the permission required by each supported method, including the extra
  `storage.objects.delete` for an `objects.insert` that overwrites a live object.
- [IAM roles for Cloud Storage](https://docs.cloud.google.com/storage/docs/access-control/iam-roles):
  the predefined role snapshot in `src/auth/roles.rs`.

## Acceptance cases

| Behavior | Expected result |
| --- | --- |
| Default profile | `permissive`; the ready event reports `"auth_mode":"permissive"`; any `Authorization` header or credential query parameter is ignored and no permission is checked. A later opt-in, `signed_urls=verify`, verifies signed XML requests first without adding permission checks; see [signed URLs](../signed-urls.md#verifying-signed-requests-in-permissive-mode) |
| Profile selection | `--auth-mode` / `GCS_MIMIC_AUTH_MODE` / JSON `auth_mode` follow the existing CLI > environment > file > default precedence; other values fail startup |
| Profile/registry pairing | `strict` without an identity document (`identity_file` or the inline `identities`) and either setting with `permissive` fail before the catalog opens or the listener binds; naming both settings fails too. Strict mode starts without a document only when the settings ask for a built-in identity, `trusted_listen` without `trusted_principal` or `test_signer=enabled`, and the error names those options. The later `signed_urls=verify` opt-in accepts the document in permissive mode for `id` and `public_keys` only; see [signed URLs](../signed-urls.md#verifying-signed-requests-in-permissive-mode) |
| Registry validation | Unknown keys, limits, duplicate IDs/tokens, invalid project IDs, unknown or basic roles and malformed/oversized files fail startup without a partial load; tokens are never echoed |
| Ready event | Strict mode reports `"auth_mode":"strict"` and the registered identity count |
| Anonymous caller | No `Authorization` header: 401 `required` with the documented example body on every supported route, including missing buckets |
| Invalid credentials | Unknown/empty/malformed/duplicate bearer values, non-UTF-8 header bytes, other schemes and credential query parameters: 401 `authError`; never anonymous fallback |
| Registered bearer token | Case-insensitive `Bearer` scheme with one or more spaces resolves the registered identity; every token of an identity resolves it |
| Project-role authorization | Each route requires its iam-json permission on the bucket's owning project; a missing permission returns 403 `forbidden` |
| Cross-project isolation | Roles granted in one project never authorize buckets owned by another project |
| Missing bucket | 404 only when the caller holds the permission in some project; otherwise 403 |
| Overwrite permission | `objects.insert` replacing a live object also needs `storage.objects.delete`; otherwise 403 without reading or staging the body, and the live object is unchanged |
| Overwrite race | Concurrent creates by a caller without `storage.objects.delete` produce exactly one 200 and 403 for the others; the winner's bytes remain |
| Precondition precedence | A failing generation precondition returns 412 before the overwrite permission check |
| Bucket recreation race | A bucket deleted and recreated in another project after authorization yields 403 with no mutation or staging residue; a still-missing bucket yields 404 for a caller with the permission somewhere |
| Check order | Authentication precedes route/query syntax validation; authorization precedes header/body validation, conditions and existence checks |
| Unauthenticated surfaces | Unknown routes keep 404 and `/_internal` probes keep 200 whatever the credentials |
| Role snapshot | 20 predefined `roles/storage.*` roles are stored as sorted permission data and evaluated by lookup, not by role name |
| Restart | The same registry authorizes persisted buckets/objects after restart |
| Public URL helper | `gcs-mimic url public` prints one encoded URL, validates every input, reads no configuration and changes no state |
| Go SDK | Pinned Go client with a static bearer token: admin lifecycle, anonymous/unregistered 401, object viewer and object creator restrictions |

## Supported subset

### Profiles and configuration

`auth_mode` is `permissive` (default) or `strict`; `identity_file` names a JSON
identity registry, and `identities` carries the same registry inline, for example
in a Compose service's `environment:`. They use the existing flat configuration
precedence: CLI `--auth-mode` / `--identity-file` / `--identities`, environment
`GCS_MIMIC_AUTH_MODE` / `GCS_MIMIC_IDENTITY_FILE` / `GCS_MIMIC_IDENTITIES`, JSON
`auth_mode` / `identity_file` / `identities`, then defaults. In the JSON config file
`identities` holds the registry object or a string that contains it. An empty
`identity_file` or `identities`, both of them together, an unknown mode, `strict`
without a registry or a registry with `permissive` fail startup before the catalog
is created or the listener binds. Strict mode without a registry is accepted only
where the settings ask for a built-in identity: `trusted_listen` without
`trusted_principal` (the application identity below), or an explicit
`test_signer=enabled`. It then starts with those identities alone and reports
`"identities":0`; otherwise a forgotten document fails loudly instead of
answering 401 to every request, and the error suggests both options. Since `signed_urls=verify`, a permissive registry
is accepted for its `id` and `public_keys` only, to verify signatures.

Both settings accept the same document, limits and validation. A load failure
names the setting that supplied the registry, `identity_file` or `identities`, and
the entry index, never a token or the document text. gcs-mimic reads an inline
value as plain text, but Compose expands `${VAR}` in it first, so write a literal
`$` as `$$`.

Permissive mode is the existing unauthenticated local profile. It is never counted
as strict coverage.

JSON CORS keeps Cloud Storage's fixed defaults. In permissive mode any page
opened in a developer's browser can read local JSON API data, including through
a loopback listener. Use strict authentication for sensitive fixtures; bucket
CORS rules do not restrict JSON API origins. Management routes separately reject
page-initiated requests before their instance authority checks.

### Identity registry

The registry is a trusted JSON file read once at startup, bounded to 16 MiB with
no limit on the number of entries in any list:

```json
{
  "identities": [
    {
      "id": "local-admin@example.invalid",
      "tokens": ["local-admin-token"],
      "project_roles": { "local-project": ["roles/storage.admin"] }
    }
  ]
}
```

| Field | Rule |
| --- | --- |
| Top level | Object with required `identities` and optional `public_access_prevention_projects`; other keys fail |
| `identities` | At least one object with `id`, `tokens` and optional `project_roles`/`public_keys`/`acl_entity` |
| `id` | 1..=254 bytes of visible ASCII (0x21..=0x7E), unique |
| `tokens` | Required array per identity, possibly empty, each token 1..=8192 bytes of visible ASCII, the bound a JWT has, unique across the registry; three dot-separated segments are rejected as JWT-shaped |
| `project_roles` | Project IDs validated like `buckets.insert`/`buckets.list`; each value is a nonempty list of distinct role names from the snapshot |
| `public_keys` | Optional nonempty array of RSA SubjectPublicKeyInfo PEM strings, each at most 4096 bytes; see the [key contract](signed-download.md#keys-and-profiles) |
| `acl_entity` | Optional canonical `user-EMAIL` or `user-ID`, at most 259 bytes; unique across every principal's explicit alias and natural user entity |
| `public_access_prevention_projects` | Optional array of distinct validated project IDs; startup-only local parent-policy simulation |

An empty token array registers a principal for separately authenticated RSA/JWT
keys or catalog-managed HMAC keys. Registration alone grants no authenticated
caller: missing credentials remain anonymous, and empty/unknown bearer tokens
or unverified JWTs are rejected. HMAC authentication requires a known ACTIVE
key and a valid signature; the principal ID is never a bearer credential.

`acl_entity` maps a local ID such as `local:operator` to an explicitly trusted
ACL user entity. Existing email or alphanumeric IDs keep their natural
`user-ID` matching. An alias cannot capture another principal's natural entity,
and group/domain/project entities are not user aliases. Requests and JWT
claims cannot choose an alias. A local ID without a canonical user form or an
explicit alias remains usable in IAM; no Google canonical identity is invented.

Unknown roles, including the basic `roles/owner`, `roles/editor` and `roles/viewer`,
fail startup. An identity without `project_roles` has no project-role permission;
separate bucket IAM and object ACL grants still apply to an authenticated caller.
Startup errors name the field and identity index; they never include a
token value. The registry never contacts Google or the metadata server.

Administrative principals use the same visible-ASCII ID format. An explicit
`admin_principal` requires strict authentication and enabled administration,
and must name an identity in the loaded registry. These checks run before
catalog creation and listener binding.

The repository fixture `config/identities.json` defines `local-admin`,
`local-viewer` and `local-creator` principals on `local-project`. Its tokens are
deliberately public emulator test values, not Google credentials.
`compose.strict.yaml` mounts it read-only and selects strict mode.

### Caller resolution

Strict mode resolves the caller from request metadata only, before any body read,
catalog access or existence check:

- No `Authorization` header is an anonymous caller.
- `Authorization: Bearer <token>` with a case-insensitive scheme, one or more spaces
  and an exactly matching registered token is that identity.
- Everything else returns 401 `authError`: unknown or empty tokens, trailing data,
  tabs, non-UTF-8 bytes, duplicate `Authorization` headers, any other scheme (Basic,
  `GOOG4-RSA-SHA256`, `GOOG4-HMAC-SHA256`, `AWS4-HMAC-SHA256`, a bare token) and the
  query parameters `access_token`, `oauth_token`, `X-Goog-Signature`,
  `X-Goog-Credential`, `X-Goog-Algorithm`, `Signature` and `GoogleAccessId`. Query
  keys are matched case-insensitively after lenient percent decoding, so an encoded
  key cannot hide a credential. These credentials never grant access or fall back
  to anonymous.

### Trusted listener

A later local extension, DESIGN-101, adds an optional `trusted_listen` for the
application's own servers. There, and only there, a request that presents no
credentials resolves to the registered `trusted_principal` instead of the
anonymous caller; everything after that is the same strict decision code, so
IAM, ACLs, public access prevention and object owners follow that identity.
Presented credentials, including invalid ones and the query parameters above,
resolve exactly as on the main listener, and a request with `Origin` or a
`Sec-Fetch-Site` other than `none` gets 403 before routing. Because a DNS
rebinding page sends neither header on a plain HTTP GET to its own rebound name,
the listener also accepts only a Host that such a page cannot use: an IP
literal, `localhost` or a `.localhost` name, a single-label name, or a name in
`trusted_allowed_hosts`. Any other Host, or none, gets 421 `misdirected`, with or
without credentials. The checks run once per request the listener receives, so
batch parts inherit their envelope's admission, and every response it sends
carries `Cross-Origin-Resource-Policy: same-origin` and
`X-Content-Type-Options: nosniff`. Both settings require
strict mode, `trusted_principal` requires `trusted_listen`, and startup checks a
named principal against the loaded registry as it does `admin_principal`. gRPC
has no trusted listener. See the
[Compose example and warnings](../container-usage.md#keep-private-buckets-private-with-a-trusted-listener).

Without `trusted_principal`, the listener's requests without credentials act as
the built-in application identity `trusted-app@gcs-mimic.invalid`, which differs
from the permissive creator `local-user@gcs-mimic.invalid` and from the test
signer. It holds `roles/storage.admin` in the effective `seed_project` and in
the project of every bucket the buckets document creates (each entry's
`project`, or else the document's top-level one, from `buckets_file` or
`buckets`). Strict mode loads authentication before the bucket initializer
runs, so configuration reads and parses that document once and keeps the
parse; the initializer validates the same text instead of reading the file
again, and a document that cannot be read or parsed fails startup there with
the initializer's diagnostic. So the application's servers can
read and write private buckets there and create buckets only in those projects;
a bucket in any other project gets 403. Name an explicit identity with
`trusted_principal` to cover other projects. The identity has no token and no
key, so no request on the main listener can act as it, and objects it writes
record it as owner like any identity. An identity document entry with its ID
replaces the implicit grants with its own roles, tokens and ACL entity: an
explicit declaration always wins. Its authority is only as private as the port:
keep the trusted listener off published ports. The ready event reports the
effective `trusted_principal`.

The built-in test signer `test-signer@gcs-mimic.invalid`, when enabled and not
declared, holds `roles/storage.objectUser` in the same projects. That role reads
and writes objects, including what Rails does, but administers no IAM policy or
ACL, because anyone can sign with the published key; see
[signed URLs](../signed-urls.md#built-in-test-signer).

### Errors

| Condition | Status | Body |
| --- | --- | --- |
| Anonymous caller needs a project-level permission (`buckets.insert`, `buckets.list`, HMAC keys) | 401 | `{"error":{"errors":[{"domain":"global","reason":"required","message":"Login Required","locationType":"header","location":"Authorization"}],"code":401,"message":"Login Required"}}` |
| Anonymous caller lacks a bucket or object permission | 401 | Reason `required`, `locationType` `header`, `location` `Authorization`, message `Anonymous caller does not have storage.objects.get access to the Google Cloud Storage object. Permission 'storage.objects.get' denied on resource (or it may not exist).` |
| Invalid or unsupported credentials | 401 | Reason `authError`, message `Invalid Credentials`, `locationType` `header`, `location` `Authorization` |
| Authenticated caller lacks the permission | 403 | `{"error":{"errors":[{"domain":"global","reason":"forbidden","message":"viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist)."}],"code":403,"message":"viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist)."}}` |

A denial message names the principal and the refused permission in the form
`<principal> does not have <permission> access to the <resource>. Permission
'<permission>' denied on resource (or it may not exist).` The principal is the
registered identity's `id`: the token holder, the `trusted_principal` on the
trusted listener, or the signer of a signed request. A caller without
credentials is `Anonymous caller`. The resource is `Google Cloud project` for
project-level permissions such as `storage.buckets.create` and
`storage.buckets.list`, `Google Cloud Storage bucket` for bucket permissions and
`storage.objects.list`, and `Google Cloud Storage object` for the other object
permissions. Replacing a live object without `storage.objects.delete` names that
permission. The message never names the bucket or object, so a denial for a
missing bucket or object reads exactly like one for an existing resource.

XML requests keep their codes: a registered caller gets 403 `AccessDenied` and
an anonymous caller 401 `AuthenticationRequired`, each with the same message in
`Message`. Refusals that no Cloud Storage permission names, such as
`projects.serviceAccount.get` without a grant in the project or an emulator
administration route, keep the documented example message `Forbidden`.
Public access prevention and retention keep their own reasons and messages.

No `WWW-Authenticate` header is sent. Rejected uploads respond before reading the
body and use the existing early-rejection close behavior; no staging file remains.

### Seeing denials in the log

Start the server with `GCS_MIMIC_LOG_LEVEL=debug` (or `--log-level debug`) to log
each denied response as one `authorization_denied` event on stderr:

```json
{"event":"authorization_denied","level":"debug","principal":"viewer@example.invalid","anonymous":false,"permission":"storage.buckets.get","resource":"bucket","bucket":"bucket-a"}
```

An anonymous denial has `"anonymous":true` and no `principal`; a project-level
denial has `"resource":"project"` and the `project` instead of the `bucket`. The
event never contains tokens, signatures, keys or object contents. A check whose
refusal does not become the response, such as one permission evaluated by
`testIamPermissions`, logs nothing. At `info` and above nothing is logged.

### Authorization

Only project-level grants are modeled. A permission is granted on a bucket when the
caller's identity has, for the bucket's owning project, a role whose snapshot
contains it.

| Route | Permission |
| --- | --- |
| `buckets.insert` | `storage.buckets.create` on `?project=` |
| `buckets.list` | `storage.buckets.list` on `?project=` |
| `buckets.get` | `storage.buckets.get` |
| `buckets.delete` | `storage.buckets.delete` |
| `objects.get` metadata and media, including `/download/storage/v1` | `storage.objects.get` |
| `objects.list` | `storage.objects.list` |
| `objects.insert` media and multipart | `storage.objects.create`, plus `storage.objects.delete` when a live object is replaced |
| `objects.patch` / `objects.update` | `storage.objects.update` |
| `objects.delete` | `storage.objects.delete` |

`projection=full` needs no extra permission because ACL and owner fields are not
rendered.

Request processing order:

1. Authenticate (401 `authError`). Probes and unknown routes skip this step.
2. Route and query syntax validation (existing 400/405/501).
3. Anonymous callers fail with 401 `required` before any catalog lookup.
4. Resolve the bucket's project and authorize (403 `forbidden`). If the bucket is
   missing, continue only when the caller holds the permission in at least one
   project; otherwise return 403.
5. Existing header/body validation, conditions and existence checks (for example
   404 for a missing bucket the caller may learn about).

The overwrite rule is enforced in two places. The advisory check before the body
is read rejects an upload that would replace a live object, and the authoritative
publication transaction rechecks it, so a concurrent create cannot be overwritten
by a caller without `storage.objects.delete`. Preconditions are evaluated first: a
failing precondition returns 412 because nothing would be replaced.

Authorization also binds the bucket owner seen during the permission check. Every
strict bucket and object catalog operation (advisory upload checks, publication,
media opens, metadata, listing and deletes) rechecks that owner inside its own
transaction. If the bucket was deleted and recreated in another project between
authorization and the operation, the request returns 403 and changes nothing. A
bucket that is still missing returns 404 for a caller holding the permission in
some project. A bucket created after a missing-bucket authorization is
re-authorized against its actual project, and an overwrite there still needs
`storage.objects.delete`.

### Role snapshot

`src/auth/roles.rs` stores 20 predefined `roles/storage.*` roles as sorted, unique
`storage.*` permission lists, with the source URL and checked date beside the data.
Beta roles listed on the page (`bucketViewer`, `annotationGeneratorService` and the
three `expressMode*` roles) are included. Non-storage permissions such as
`resourcemanager.projects.get` are omitted because no route evaluates them.
Permission checks look up data; no role name is special-cased. Notable consequences
that tests assert:

- `roles/storage.objectViewer` reads and lists objects but cannot get bucket metadata.
- `roles/storage.objectCreator` creates new objects but cannot overwrite or read them.
- `roles/storage.editor` creates and lists buckets but lacks `storage.buckets.get`.
- `roles/storage.objectAdmin` cannot create buckets.

### Public URL helper

`gcs-mimic url public --base-url URL --bucket NAME --object NAME` prints exactly one
line, `<base-url without trailing slashes>/<bucket>/<encoded object>`, and exits 0.
Each option is required exactly once, in any order; `url public --help` prints usage.
The base URL follows the `api_base_url` rules (absolute HTTP(S), optional path
prefix, no credentials, query or fragment). The bucket and object use the existing
name validators. Invalid input exits nonzero with a message on stderr and nothing
on stdout.

Every UTF-8 byte except RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _ ~`) and
`/` is percent-encoded with uppercase hex. If any `/`-separated segment is `.` or
`..`, every `/` is also encoded as `%2F`, so clients and proxies cannot remove or
collapse dot segments. Empty segments are preserved.

The helper is a pure function: it reads no configuration file, environment
variable, data directory or network, and it changes no permission. The URL
addresses the XML-style path endpoint, which this slice does not serve: requesting
it currently returns 404 as an unknown route. Serving it requires the next slice's
`allUsers` grants and XML GET support.

## Decisions and local rules

- The identity registry is JSON, not the TOML sketched in the future onboarding
  specification, because the runtime configuration is already bounded flat JSON.
- Strict authentication covers only the Google JSON API routes. Probes and unknown
  routes are not authenticated, so unknown routes keep 404 for every credential.
- Authentication precedes route syntax validation; an anonymous request with an
  invalid bucket name still receives 400, while an invalid token receives 401.
- This slice modeled no anonymous grants, so anonymous callers needed a login
  here. The [public object reads](public-object-reads.md) slice later added
  `allUsers` grants: strict anonymous requests now match `allUsers` grants
  (never `allAuthenticatedUsers`), and a JSON request that no grant authorizes
  still receives 401 `required`.
- The missing-bucket 404/403 rule is a local information-hiding choice.
- For an overwrite, generation/metageneration preconditions (412) are evaluated
  before the overwrite permission check (403). A caller without
  `storage.objects.delete` can therefore distinguish a matching from a mismatching
  generation; the real-service order is unverified.
- Tokens are compared by hash-map lookup, not in constant time.
- Duplicate JSON keys in the registry follow serde_json's last-value-wins behavior.

## Deviations

[SPEC-017](../../compatibility/gaps.json) records the open questions:

- The `authError` message and its `locationType`/`location` are local choices; only
  the reason is documented.
- The status-code reference documents only the short `Forbidden` and
  `Login Required` examples. The denial message follows Google's
  [permission error messages](https://docs.cloud.google.com/iam/docs/permission-error-messages)
  page, which shows `EMAIL_ADDRESS does not have storage.buckets.list access to
  the Google Cloud project. Permission 'storage.buckets.list' denied on resource
  (or it may not exist).`, and the
  [troubleshooting](https://docs.cloud.google.com/storage/docs/troubleshooting)
  page, which shows `example@email.com doesn't have storage.objects.get access to
  the Google Cloud Storage object` and mentions `Anonymous caller`. The
  `Google Cloud Storage bucket` noun, the second sentence on bucket and object
  denials and the complete anonymous sentence are local choices, as is the 401
  for anonymous bucket and object denials. A recorded `objects.move` denial
  named the resource path in its second sentence
  (`denied on resource '//<host>/projects/_/buckets/<bucket>/objects/<name>'`);
  the emulator's other denials omit it.
- No `WWW-Authenticate` header is sent; its presence on the real service is
  undocumented.
- The missing-bucket rule, the anonymous result for missing buckets, the absence
  of extra permissions for `projection=full` and the 412-before-403 overwrite order
  need authorized real-service evidence.

## Limitations

Not implemented in this slice: bucket/object IAM policies, ACLs and default ACLs,
`allUsers`/`allAuthenticatedUsers`, uniform bucket-level access, public access
prevention, signed URLs (V2/V4), HMAC keys, service-account signing, IAM conditions,
custom roles, basic roles, real OAuth token verification and XML endpoints. Real
Google credentials are never accepted as such: an unregistered bearer token is
rejected with 401. Tokens are not constant-time compared, and duplicate registry
keys are last-value-wins. No performance claim is made.

DESIGN-002 and the JSON method rows remain PARTIAL. [Evidence](../evidence/strict-auth.md)
lists the verification commands and logs.

## Getting-started snippet

The runnable guide's [Strict local authentication](../getting-started.md#strict-local-authentication)
section starts the Compose service with `compose.strict.yaml`, shows 401/403/200
outcomes for anonymous, unregistered, admin, viewer and creator callers, builds a
public URL and confirms that it is not yet served.
