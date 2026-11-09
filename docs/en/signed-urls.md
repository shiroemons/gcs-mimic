# Signed private downloads and uploads

<!-- markdownlint-configure-file {"MD013": false} -->

Part of the [setup and integration guides](getting-started.md#guides). Run the
shell blocks from the repository root in one Bash session.

This standalone journey needs Bash, curl, jq and OpenSSL. Run it from the repository
root. It uses a disposable strict registry/catalog and a local RSA key; it never
calls Google IAM. The server receives only the public key. Native and source-built
Docker paths run the same private GET/PUT, response overrides, virtual-host and
PAP checks. HTTPS clients must add the CA exported in the [local HTTPS guide](local-https.md).

Virtual-host routing is disabled by default. This journey enables the explicit
`localhost` suffix with `--xml-virtual-host-domains localhost` natively and
`GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS: localhost` in Compose. The equivalent JSON
configuration is `{"xml_virtual_host_domains":"localhost"}`; CLI overrides
environment, then file settings. Only bucket hosts under configured suffixes
route as XML. JSON APIs and probes stay on the base endpoint.
DNS must resolve each final bucket host to the listener. Some clients resolve
`*.localhost` automatically; the commands use curl `--resolve` to work without
system DNS changes. This changes only the connection destination, preserving
the signed URL, Host and HTTPS SNI.

For HTTPS virtual hosts, supply `--tls-cert` and `--tls-key` plus client CA trust.
The built-in certificate does not cover bucket subdomains. A SAN must match the
final host, for example `signed-guide-bucket.localhost`; `*.localhost` covers
one bucket label, not a dotted bucket name. Dotted virtual bucket names are an
explicit local extension (SPEC-XML-VIRTUAL-HOST). Never disable certificate
verification or change the host after signing.

```sh
signed_work=$(mktemp -d)
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
  -out "$signed_work/private.pem" 2> "$signed_work/keygen.log"
openssl pkey -in "$signed_work/private.pem" -pubout -out "$signed_work/public.pem"
jq -n --rawfile key "$signed_work/public.pem" '{identities:[{
  id:"guide-signer@example.invalid", tokens:["guide-admin-token"], public_keys:[$key],
  project_roles:{"local-project":["roles/storage.admin"]}
}]}' > "$signed_work/identities.json"
printf 'Signed private bytes.\n' > "$signed_work/input.txt"
signed_sign() {
  "$signed_binary" url sign --base-url "$2" --bucket signed-guide-bucket \
    --object "$3" --method "$1" --expires 5m --signer guide-signer@example.invalid \
    --private-key "$signed_work/private.pem" "${@:4}"
}
signed_curl() {
  curl --disable --noproxy '*' --silent --show-error --max-time 15 "$@"
}
signed_journey() {
  signed_curl --fail -H 'Authorization: Bearer guide-admin-token' \
    -H 'Content-Type: application/json' --data '{"name":"signed-guide-bucket"}' \
    "$signed_endpoint/storage/v1/b?project=local-project" > "$signed_work/bucket.json"
  signed_curl --fail -H 'Authorization: Bearer guide-admin-token' \
    --data-binary @"$signed_work/input.txt" \
    "$signed_endpoint/upload/storage/v1/b/signed-guide-bucket/o?uploadType=media&name=private.txt" \
    > "$signed_work/object.json"
  test "$(signed_curl -o /dev/null -w '%{http_code}' \
    "$signed_endpoint/signed-guide-bucket/private.txt")" = 401
  signed_get=$(signed_sign GET "$signed_endpoint" private.txt)
  signed_curl --fail "$signed_get" > "$signed_work/download.txt"
  cmp "$signed_work/input.txt" "$signed_work/download.txt"
  signed_put=$(signed_sign PUT "$signed_endpoint" uploaded.txt \
    --header 'content-type:text/plain' --header 'x-goog-if-generation-match:0')
  signed_curl --fail -X PUT -H 'Content-Type: text/plain' \
    -H 'x-goog-if-generation-match: 0' --data-binary @"$signed_work/input.txt" "$signed_put"
  uploaded_get=$(signed_sign GET "$signed_endpoint" uploaded.txt)
  signed_curl --fail "$uploaded_get" > "$signed_work/uploaded.txt"
  cmp "$signed_work/input.txt" "$signed_work/uploaded.txt"
  test "$(signed_curl -o /dev/null -w '%{http_code}' -X PUT "$signed_get")" = 403
  changed_object=${signed_get/\/private.txt/\/changed.txt}
  test "$(signed_curl -o /dev/null -w '%{http_code}' "$changed_object")" = 403
  test "$(signed_curl -o /dev/null -w '%{http_code}' \
    -H 'Host: changed.invalid' "$signed_get")" = 403
  expired_get=$(signed_sign GET "$signed_endpoint" private.txt --timestamp 20200101T000000Z)
  test "$(signed_curl -o /dev/null -w '%{http_code}' "$expired_get")" = 400
  test "$(signed_curl -o /dev/null -w '%{http_code}' -X PUT \
    -H 'Content-Type: changed' -H 'x-goog-if-generation-match: 0' "$signed_put")" = 403
  test "$(signed_curl -o /dev/null -w '%{http_code}' -X PUT \
    -H 'Content-Type: text/plain' -H 'x-goog-if-generation-match: 0' \
    --data-binary @"$signed_work/input.txt" "$signed_put")" = 412
  echo 'PASS: private signed GET/PUT, method/object/host/expiry/header failures and atomic condition'

  override_get=$(signed_sign GET "$signed_endpoint" private.txt \
    --query 'response-content-type=text/csv' \
    --query 'response-content-disposition=attachment; filename="private.csv"')
  signed_curl --fail -D "$signed_work/override.headers" "$override_get" \
    > "$signed_work/override.txt"
  cmp "$signed_work/input.txt" "$signed_work/override.txt"
  tr -d '\r' < "$signed_work/override.headers" | grep -Fxi 'content-type: text/csv'
  tr -d '\r' < "$signed_work/override.headers" | \
    grep -Fxi 'content-disposition: attachment; filename="private.csv"'
  test "$(signed_curl -o /dev/null -w '%{http_code}' \
    "${override_get/text%2Fcsv/text%2Fhtml}")" = 403
  echo 'PASS: signed response overrides and query tamper rejection'

  signed_curl --fail -H 'Authorization: Bearer guide-admin-token' \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket/iam" > "$signed_work/policy.json"
  jq '.bindings += [{role:"roles/storage.objectViewer",members:["allUsers"]}]' \
    "$signed_work/policy.json" > "$signed_work/public-policy.json"
  signed_curl --fail -X PUT -H 'Authorization: Bearer guide-admin-token' \
    -H 'Content-Type: application/json' --data-binary @"$signed_work/public-policy.json" \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket/iam" > "$signed_work/public-policy.json.new"
  signed_curl --fail "$signed_endpoint/signed-guide-bucket/private.txt" \
    > "$signed_work/public.txt"
  cmp "$signed_work/input.txt" "$signed_work/public.txt"

  signed_curl --fail -X PATCH -H 'Authorization: Bearer guide-admin-token' \
    -H 'Content-Type: application/json' \
    --data '{"iamConfiguration":{"publicAccessPrevention":"enforced"}}' \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket" > "$signed_work/pap.json"
  jq -e '.iamConfiguration.publicAccessPrevention == "enforced"' "$signed_work/pap.json" > /dev/null
  test "$(signed_curl -o /dev/null -w '%{http_code}' \
    "$signed_endpoint/signed-guide-bucket/private.txt")" = 401
  pap_get=$(signed_sign GET "$signed_endpoint" private.txt)
  signed_curl --fail "$pap_get" > "$signed_work/pap-download.txt"
  cmp "$signed_work/input.txt" "$signed_work/pap-download.txt"
  pap_put=$(signed_sign PUT "$signed_endpoint" pap-uploaded.txt \
    --header 'content-type:text/plain' --header 'x-goog-if-generation-match:0')
  signed_curl --fail -X PUT -H 'Content-Type: text/plain' \
    -H 'x-goog-if-generation-match: 0' --data-binary @"$signed_work/input.txt" "$pap_put"
  pap_uploaded_get=$(signed_sign GET "$signed_endpoint" pap-uploaded.txt)
  signed_curl --fail "$pap_uploaded_get" > "$signed_work/pap-uploaded.txt"
  cmp "$signed_work/input.txt" "$signed_work/pap-uploaded.txt"
  test "$(signed_curl -o /dev/null -w '%{http_code}' "${pap_get}00")" = 403
  echo 'PASS: enforced PAP denies anonymous access and permits valid signed GET/PUT'
  signed_curl --fail -H 'Authorization: Bearer guide-admin-token' \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket/iam" > "$signed_work/pap-policy.json"
  cmp "$signed_work/public-policy.json.new" "$signed_work/pap-policy.json"

  signed_port=${signed_endpoint##*:}
  virtual_base="http://localhost:$signed_port"
  virtual_host="signed-guide-bucket.localhost:$signed_port"
  virtual_get=$(signed_sign GET "$virtual_base" private.txt --style virtual-hosted \
    --query 'response-content-type=text/csv')
  signed_curl --fail --resolve "$virtual_host:127.0.0.1" \
    -D "$signed_work/virtual.headers" "$virtual_get" > "$signed_work/virtual.txt"
  cmp "$signed_work/input.txt" "$signed_work/virtual.txt"
  tr -d '\r' < "$signed_work/virtual.headers" | grep -Fxi 'content-type: text/csv'
  test "$(signed_curl --resolve "$virtual_host:127.0.0.1" -o /dev/null -w '%{http_code}' \
    "http://$virtual_host/private.txt")" = 401
  virtual_put=$(signed_sign PUT "$virtual_base" virtual-uploaded.txt --style virtual-hosted \
    --header 'content-type:text/plain' --header 'x-goog-if-generation-match:0')
  signed_curl --fail --resolve "$virtual_host:127.0.0.1" -X PUT \
    -H 'Content-Type: text/plain' -H 'x-goog-if-generation-match: 0' \
    --data-binary @"$signed_work/input.txt" "$virtual_put"
  virtual_uploaded_get=$(signed_sign GET "$virtual_base" virtual-uploaded.txt --style virtual-hosted)
  signed_curl --fail --resolve "$virtual_host:127.0.0.1" "$virtual_uploaded_get" \
    > "$signed_work/virtual-uploaded.txt"
  cmp "$signed_work/input.txt" "$signed_work/virtual-uploaded.txt"
  echo 'PASS: virtual-host signed GET/PUT and response override under PAP'

  signed_curl --fail -X PATCH -H 'Authorization: Bearer guide-admin-token' \
    -H 'Content-Type: application/json' \
    --data '{"iamConfiguration":{"publicAccessPrevention":"inherited"}}' \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket" > "$signed_work/pap-disabled.json"
  jq -e '.iamConfiguration.publicAccessPrevention == "inherited"' "$signed_work/pap-disabled.json" > /dev/null
  signed_curl --fail -H 'Authorization: Bearer guide-admin-token' \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket/iam" > "$signed_work/reactivated-policy.json"
  cmp "$signed_work/public-policy.json.new" "$signed_work/reactivated-policy.json"
  signed_curl --fail "$signed_endpoint/signed-guide-bucket/private.txt" > "$signed_work/reactivated.txt"
  cmp "$signed_work/input.txt" "$signed_work/reactivated.txt"
  signed_curl --fail --resolve "$virtual_host:127.0.0.1" "http://$virtual_host/private.txt" \
    > "$signed_work/reactivated-virtual.txt"
  cmp "$signed_work/input.txt" "$signed_work/reactivated-virtual.txt"
  echo 'PASS: stored public policy survives PAP and reactivates on both endpoint styles'
  signed_curl --fail -X PATCH -H 'Authorization: Bearer guide-admin-token' \
    -H 'Content-Type: application/json' \
    --data '{"iamConfiguration":{"publicAccessPrevention":"enforced"}}' \
    "$signed_endpoint/storage/v1/b/signed-guide-bucket" > /dev/null
}
```

Build and run the native binary. The ephemeral listener differs from the default
advertised API base; the helper receives the actual final client endpoint.

```sh
cargo build --locked
signed_binary=./target/debug/gcs-mimic
env -u GCS_MIMIC_IMAGE -u GCS_MIMIC_PORT \
  "$signed_binary" serve --scheme http --listen 127.0.0.1:0 \
  --xml-virtual-host-domains localhost \
  --data-dir "$signed_work/native-data" --auth-mode strict \
  --identity-file "$signed_work/identities.json" > "$signed_work/native.log" 2>&1 &
signed_pid=$!
for attempt in {1..100}; do
  signed_address=$(jq -r 'select(.event == "ready") | .listen' "$signed_work/native.log")
  if [ -n "$signed_address" ]; then break; fi
  sleep 0.1
done
signed_endpoint="http://$signed_address"
"$signed_binary" healthcheck --url "$signed_endpoint/_internal/ready"
signed_journey
kill -TERM "$signed_pid"
wait "$signed_pid"
```

First build the source image using the
[Compose build block](getting-started.md#build-and-start-with-compose). Retain the
same `GCS_MIMIC_IMAGE` if you selected a custom tag. This isolated project uses
the strict override with its registry mount replaced by the generated
`identities.json`. The server never mounts `private.pem`; only the short-lived,
network-disabled signing container reads it.

```sh
signed_project="gcs-mimic-signed-$$"
export GCS_MIMIC_PORT=0
cat > "$signed_work/compose.signed.yaml" <<EOF
services:
  gcs:
    environment:
      GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS: localhost
    volumes:
      - type: bind
        source: $signed_work/identities.json
        target: /etc/gcs-mimic/identities.json
        read_only: true
EOF
signed_compose() {
  docker compose -p "$signed_project" -f compose.yaml -f compose.http.yaml \
    -f compose.strict.yaml -f "$signed_work/compose.signed.yaml" "$@"
}
signed_compose up -d --no-build --wait
signed_endpoint="http://$(signed_compose port gcs 4443)"
signed_sign() {
  docker run --rm --network none --read-only --user "$(id -u):$(id -g)" \
    --mount "type=bind,src=$signed_work/private.pem,dst=/key.pem,readonly" \
    "${GCS_MIMIC_IMAGE:-gcs-mimic:dev}" url sign --base-url "$2" \
    --bucket signed-guide-bucket --object "$3" --method "$1" --expires 5m \
    --signer guide-signer@example.invalid --private-key /key.pem "${@:4}"
}
signed_journey
signed_compose stop
signed_compose start --wait
signed_endpoint="http://$(signed_compose port gcs 4443)"
test "$(signed_curl -o /dev/null -w '%{http_code}' \
  "$signed_endpoint/signed-guide-bucket/private.txt")" = 401
uploaded_get=$(signed_sign GET "$signed_endpoint" uploaded.txt)
signed_curl --fail "$uploaded_get" > "$signed_work/restarted.txt"
cmp "$signed_work/input.txt" "$signed_work/restarted.txt"
peer_get=$(signed_sign GET http://gcs:4443 uploaded.txt)
peer_image=curlimages/curl:8.23.0@sha256:3fb7e13e5d179a845c97d78204f3f1064d1907037f26a3ebf91710571151fd93
docker run --rm --read-only --network "${signed_project}_default" "$peer_image" \
  --disable --noproxy '*' --silent --show-error --fail --max-time 15 "$peer_get" \
  > "$signed_work/peer.txt"
cmp "$signed_work/input.txt" "$signed_work/peer.txt"
echo 'PASS: signed Docker restart and independently signed Compose peer URL'
signed_compose down --volumes
rm -r "$signed_work"
unset GCS_MIMIC_PORT
```

The host PC and browser use the published loopback port; a Compose peer uses
`gcs:4443`. A browser cannot resolve that Compose-only name. Sign a separate URL
for each final host/port and never rewrite it afterward. With a dynamically
published port, inspect the port again after restart and create a fresh signature.
Signing changes no IAM policy. A signer needs create for new uploads and delete
as well for overwrites. An application server that reaches the emulator through a
[trusted listener](container-usage.md#keep-private-buckets-private-with-a-trusted-listener)
such as `gcs:4444` still signs for the browser-facing address: that port is not
reachable from the browser, and a signed request authenticates as its signer on
every listener.
SHA256/Content-MD5 failures preserve the old object; sign headers to constrain
metadata, digest declarations and conditions. SHA256 checking is incremental;
the canonical URL payload remains UNSIGNED-PAYLOAD. CORS grants no permission.
The [trusted HTTPS Chromium journey](evidence/signed-browser.md) verifies actual
signed PUT/GET/HEAD, exposed headers, CORS denials, strict errors and restart.

Go storage v1.70.0 omits non-default ports from canonical Host. A server that
verifies signatures, like the strict journey above, therefore rejects its URLs
for a host:port endpoint with 403 `SignatureDoesNotMatch`; when a signature
matches the request only without its port, the message says so and suggests
`signed_host_port=lenient`. Without the setting, the enabled
[built-in test signer](#built-in-test-signer) gets that tolerance for its own
signatures: its published key already marks a local test, so tolerating a
signer that drops the port changes nothing that matters there. A registered key
or an HMAC key still has to sign the port under that default. The default
permissive server does not check
signatures, so those URLs work there. Prefer the helper
for local ports or expose a default-port endpoint. Where that is impractical,
for example when every worktree publishes a different port, start a server that
verifies signatures (strict mode, or permissive mode with `signed_urls=verify`;
elsewhere an explicit `lenient` is a startup error because it would change
nothing) with `--signed-host-port lenient` (`GCS_MIMIC_SIGNED_HOST_PORT=lenient`,
JSON `signed_host_port`), which covers every signer, or sign with the test
signer. An explicit `exact` wins over that default, for the test signer too, and
is accepted everywhere. The ready event reports the effective value as
`signed_host_port` (`exact` or `lenient`) and, while it is lenient, whose
signatures it covers as `signed_host_port_signers`: `all` for the explicit
setting, `test-signer` for the default. A V4 signature that does not match the actual Host is then
checked once more with the port removed from the canonical host and accepted
only if that matches. The host name, method, path, query, signed headers,
payload rule and time window stay bound, and an IPv6 literal keeps its brackets.
V2 signatures and POST policies do not sign Host and are unaffected. The first
such match in a process logs one `signature_host_port_omitted` warning without
the URL or credentials. `exact`, the default without the test signer, keeps
verification unchanged. The Go SDK suite sends unmodified `SignedURL` GET and
PUT URLs for a host:port to a strict server started with `lenient` and the
[built-in test signer](#built-in-test-signer), where they pass while tampering
still fails, and checks that an exact server rejects such a URL with the
message above; see the
[evidence](evidence/signed-urls.md#built-in-test-signer-and-lenient-host-ports-with-official-sdks).
The Node.js signer drops the port the same way; see the
[Node.js SDK notes](../../sdk-tests/node/README.md). Ruby Storage 1.62.1 signs
`storage.googleapis.com` by default even with an emulator endpoint. Its pinned
test uses `file.signed_url(version: :v4, method: "GET"/"PUT", issuer:, signing_key:,
bucket_bound_hostname:)` with an explicit bucket/name File path because
bucket-bound signing removes the normal bucket prefix. This preserves host:port
before signing; see the [SDK details](../../sdk-tests/ruby/README.md) and
SPEC-GO-SIGNER-PORT / SPEC-RUBY-SIGNER-ENDPOINT. Exact verification stays the
default where the test signer is disabled.
PAP suppresses stored `allUsers` and `allAuthenticatedUsers` grants in strict mode;
explicit signer grants remain usable. The journey compares the policy before,
during and after enforcement, shows public reactivation with `inherited`, then
enforces again for the restart check. A configured local parent fixture can keep
an inherited bucket effectively enforced; see the [PAP contract](slices/public-access-prevention.md).

Only `response-content-type` and `response-content-disposition` are supported.
Supply literal values through repeated `url sign --query NAME=VALUE`; the helper
encodes them before signing. These change response headers without mutating
stored metadata. Query changes invalidate the signature. Existing V2/V4
signing parameter names are rejected case-insensitively,
including `Expires`, `Signature` and `GoogleAccessId`. Query names supplied to
`--query` are literal; percent signs are encoded like other data. Duplicate decoded names
are rejected locally, while exact provider duplicate/validation behavior stays
open in SPEC-SIGNING-EDGES and SPEC-XML-RESPONSE-OVERRIDES. Other response-* names
return 501. The [signed-browser subsection](getting-started.md#signed-browser-upload-and-download)
documents the separate actual-browser preparation/run workflow.

When using the pinned SDK signers, omit x-goog-content-sha256/SHA256 signing
options for PUT URLs. Go v1.70.0 substitutes a supplied header into the canonical
payload and the emulator rejects that URL with 403 SignatureDoesNotMatch, even
for a matching body; no object is changed. To bind SHA256 locally, use
`gcs-mimic url sign --header 'x-goog-content-sha256:LOWERCASE_HEX_DIGEST'` and send
the same header. The helper keeps the official query UNSIGNED-PAYLOAD rule and
the server checks the streamed bytes. See SPEC-SIGNED-PAYLOAD.

## Query parameter name case

V4 signing query parameters (`X-Goog-*` and `X-Amz-*`) match in any ASCII case,
because `gsutil signurl` and `gcloud storage sign-url` send them in lowercase. The
canonical query keeps each name exactly as sent and sorts by those bytes, so a URL
must be signed in the spelling it is sent in. Two spellings of one parameter are
rejected, and so is mixing the Google and AWS families. V2 parameters
(`GoogleAccessId`, `Expires`, `Signature`) remain case sensitive.

## Built-in test signer

> **Warning:** the built-in test signer's private key is public. It is
> committed to this repository as `fixtures/signing/test-only-private.pem` and
> embedded in every gcs-mimic binary and image, so anyone can sign as this
> identity. Use it only for local tests, never on a shared or exposed server,
> and never as a Google credential.

The built-in test signer is the identity `test-signer@gcs-mimic.invalid`,
whose public key is `fixtures/signing/test-only-public.pem`. Apps and tests can
sign URLs with it instead of generating and registering a key. `test_signer`
(CLI `--test-signer`, environment `GCS_MIMIC_TEST_SIGNER`, JSON `test_signer`)
is `enabled` or `disabled`. Its default depends on the authentication profile,
and the ready event reports the effective value as `"test_signer"`:

| Profile | Default | Why |
| --- | --- | --- |
| `permissive` with `signed_urls=verify` | `enabled` | Permissive mode authorizes nothing by identity, so the signer grants nothing |
| `permissive` with the default `signed_urls=ignore` | `disabled` | No signature is verified, so the signer would do nothing |
| `strict` whose identity document declares `test-signer@gcs-mimic.invalid` | `enabled` | The entry for its ID says that this local setup asked for the signer |
| `strict` otherwise | `disabled` | Anyone can sign with the published key, so the signer would turn `allAuthenticatedUsers` grants into public access |

An explicit value wins. An explicit `disabled` is accepted everywhere. An
explicit `enabled` in permissive mode with `signed_urls=ignore` fails startup
("test_signer=enabled has no effect while signatures are ignored"), because it
would promise a verification that never happens. The identity document is
`identity_file` or the inline `identities`; either one declaring the ID counts.

- **Strict mode with the signer enabled**, by `test_signer=enabled` or by an
  entry for its ID: startup emits one warning before readiness that the
  signer's private key is published, so anyone can authenticate as that
  identity and reach `allAuthenticatedUsers` grants and anything granted to it.
  The warning names the input that enabled it: the `test_signer` input, or the
  `identity_file` or `identities` input that declares the entry. The identity
  then exists with the built-in public key.
  If the identity document declares `test-signer@gcs-mimic.invalid`, that entry
  supplies its tokens, project roles and ACL entity, and the built-in key is
  added to its `public_keys`: an explicit declaration replaces every implicit
  grant. Otherwise it holds `roles/storage.objectUser` in the effective
  `seed_project` and in the project of every bucket the buckets document
  (`buckets_file` or `buckets`) creates, and the warning says so. That is the least predefined role
  that covers signed GET and PUT, overwrites and what Rails also does (delete,
  list and metadata updates), and it administers no IAM policy or ACL, which
  matters because anyone can sign with the published key. An object it writes
  records it as owner, so with fine-grained ACLs it may still edit that object's
  own ACL, as an owner may in Cloud Storage; uniform bucket-level access removes
  that. In other projects a bucket or object grant such as
  `user:test-signer@gcs-mimic.invalid` authorizes it, and a signature it makes
  without one gets 403 `AccessDenied`. V4 and V2 RSA
  signed URLs and self-signed service-account JWTs from it verify like those of
  any registered identity with public keys. An identity entry whose ACL
  alias or natural user entity names the signer fails startup unless it
  declares the signer itself.
- **Strict mode without an entry for its ID, or with `test_signer=disabled`**:
  there is no built-in signer. With `test_signer=disabled`, an entry with its ID
  is an ordinary identity with only its own keys. Signatures made with the
  published key fail with 403 `InvalidAccessKeyId`, whose message says that the
  built-in test signer is disabled and names `test_signer=enabled` (or with 403
  `SignatureDoesNotMatch` when that entry declares other `public_keys`), and its
  JWTs with 401.
- **Permissive mode** ignores signatures by default, so the signer is disabled.
  With `signed_urls=verify` the built-in key verifies them unless
  `test_signer=disabled`; see
  [verifying signed requests in permissive mode](#verifying-signed-requests-in-permissive-mode).

The ready event's `identities` counts only the identity document's entries. To
use the signer with strict mode, declare it in the identity document with the
project roles it needs, or enable it explicitly:

```sh
GCS_MIMIC_AUTH_MODE=strict GCS_MIMIC_IDENTITY_FILE=identities.json \
  GCS_MIMIC_TEST_SIGNER=enabled gcs-mimic serve
```

`gcs-mimic url sign` signs as the built-in signer when both `--signer` and
`--private-key` are omitted; giving only one of them is an error:

```sh
gcs-mimic url sign --base-url http://localhost:4443 --bucket my-bucket \
  --object my-object --method GET --expires 15m
```

`gcs-mimic test-signer credentials` prints the signer as a service account key
file, so binary and image users need no repository checkout. It contains
`type`, `project_id` (`local-project`), `client_email`, `private_key` and the
non-Google `universe_domain` (`gcs-emulator.invalid`) from the
[Rails guide](rails-active-storage.md#2-add-an-emulator-entry-to-configstorageyml),
which keeps Google auth libraries from requesting tokens from Google. The
command reads no configuration, environment or network. The image's entrypoint
is `gcs-mimic`, so `docker run` takes the same arguments; 0.13.0 and earlier
images do not have the command:

```sh
gcs-mimic test-signer credentials > test-signer.json
docker run --rm "${GCS_MIMIC_IMAGE:-gcs-mimic:dev}" test-signer credentials > test-signer.json
jq -r .private_key test-signer.json > test-signer.pem
```

Go signs with the same key through `SignedURLOptions`:

```go
key, err := os.ReadFile("test-signer.pem")
if err != nil {
    return err
}
url, err := storage.SignedURL("my-bucket", "my-object", &storage.SignedURLOptions{
    Scheme:         storage.SigningSchemeV4,
    GoogleAccessID: "test-signer@gcs-mimic.invalid",
    PrivateKey:     key,
    Method:         "GET",
    Expires:        time.Now().Add(15 * time.Minute),
    Hostname:       "localhost:4443",
    Insecure:       true,
})
```

Go storage drops a non-default port from the Host it signs, so a URL for
`localhost:4443` does not match the request's `Host: localhost:4443`. Exact
verification rejects it with 403 `SignatureDoesNotMatch`, but a server with the
built-in signer enabled tolerates the missing port in that signer's signatures
by default, so the URL above verifies there unless `signed_host_port=exact` is
set. Elsewhere, and for any other key, use a default-port endpoint, sign with
`gcs-mimic url sign`, or set `signed_host_port=lenient`. The Go SDK suite signs this way with the
printed key for a `127.0.0.1` host:port and passes against a strict server with
the test signer enabled and `signed_host_port=lenient`.

Rails Active Storage takes the same credentials in `config/storage.yml`. Save
the key in the app, for example with
`gcs-mimic test-signer credentials | jq -r .private_key > config/gcs-mimic-test-signer.pem`:

```yaml
gcs_emulator:
  service: GCS
  project: local-project
  bucket: local-bucket
  endpoint: http://localhost:4443/
  universe_domain: gcs-emulator.invalid
  credentials:
    type: service_account
    project_id: local-project
    client_email: test-signer@gcs-mimic.invalid
    universe_domain: gcs-emulator.invalid
    private_key: <%= File.read(Rails.root.join("config/gcs-mimic-test-signer.pem")).dump if Rails.env.local? %>
```

In strict mode, start the server with `GCS_MIMIC_TEST_SIGNER=enabled`, which
also lets strict mode start without an identity document: the signer then reads
and writes objects in the buckets document's projects and `seed_project`, and a
bucket elsewhere needs a grant for `user:test-signer@gcs-mimic.invalid`. To
choose its roles yourself, declare the signer with project roles in the
identity document, which also enables it. The strict Rails suite
(`sdk-tests/ruby/active_storage/run-strict.sh`) runs this recipe against a
server whose identity file gives the signer only `roles/storage.objectUser`:
Rails' JWT bearer, its V2 URLs and the final-origin V4 adapter pass. A
[trusted listener](container-usage.md#keep-private-buckets-private-with-a-trusted-listener)
does not replace the signer for Rails, because Rails always sends its own token.

## Legacy V2 HMAC and header signatures

Strict XML accepts HMAC V2 query credentials
(`GoogleAccessId`, `Expires`, `Signature`) and `GOOG1`/`AWS` Authorization
headers. They use HMAC-SHA1 over the literal UTF-8 secret and legacy canonical
request, with a base64 signature. Each request resolves an ACTIVE key and its
registered signer before shared IAM/ACL, PAP and UBLA authorization. Key-admin
privileges and a key's project do not grant object access.

Object GET/HEAD/PUT/DELETE support path-style and configured virtual-host
resources. Virtual hosts sign `/BUCKET/OBJECT` while sending `/OBJECT`;
generation selection remains rejected. Header signatures bind documented
response overrides and preserve literal `+` in query values. Google V2 query
signatures retain Google's explicitly unsigned response overrides. Header date
validation uses a local inclusive 15-minute window; exact Google legacy error
parity remains open. JSON does not accept these signature credentials.

See the [contract, independent vectors and remaining scope](slices/xml-v2-hmac-signatures.md).
Broader forms, signed resumable operations, virtual bucket query edges and
strict SDK/provider verification remain separate pending work.

## Signed header chunk uploads

Strict XML object PUT supports `AWS4-HMAC-SHA256` Authorization with the
`STREAMING-AWS4-HMAC-SHA256-PAYLOAD` marker, an ACTIVE local HMAC key,
`x-amz-decoded-content-length` and signed data/zero chunks. Use
`Content-Encoding: aws-chunked`; HTTP Content-Length counts the encoded body,
while the object-size limit and Content-MD5 count decoded stored bytes. Outer
HTTP chunked framing is also accepted. The aws-chunked token is removed from
stored encoding metadata; other encodings and their bytes are preserved.

Google forbids x-goog extension headers on the signed chunked transfer path.
Do not mix query/header credentials or substitute Google signing prefixes for
the linked AWS chunk algorithm. No-encoding HTTP chunked cases and remaining
provider/operation limits are explicit in the
[signed chunk upload evidence](slices/xml-signed-chunked-upload.md). A failed
chunk, zero signature, decoded length or transport EOF cannot publish an object.

## S3-compatible extension headers

Cloud Storage [processes `x-amz-*` headers that have an `x-goog-*`
equivalent](https://docs.cloud.google.com/storage/docs/aws-simple-migration) when a
request carries an AWS signature. Under a V2 `AWS` or `AWS4-HMAC-SHA256` signature,
in the `Authorization` header or the query, these are read as the `x-goog-*` header
with the same suffix after the signature is verified over the original names:

- `x-amz-meta-*`, `x-amz-acl`, `x-amz-storage-class`, `x-amz-metadata-directive`
- `x-amz-copy-source` and its `-if-match`, `-if-none-match`, `-if-modified-since`
  and `-if-unmodified-since` conditions
- `x-amz-project-id` and `x-amz-date`

The translated header then behaves exactly as its Google name does, so an
unsupported one, such as a source date condition, stays rejected. Reads answer with
custom metadata as `x-amz-meta-*`. Sending both spellings of one header is 400
`ExcessHeaderValues`. Other `x-amz-*` headers, such as `x-amz-server-side-encryption`,
`x-amz-grant-*` and unknown names, are accepted without effect. The customer-key
encryption headers stay rejected: Cloud Storage answered 400
`MalformedSecurityHeader` when they were not signed and 403 when they were.
Where signatures are ignored, the form of the credentials still selects this behavior,
so an S3 client with a made-up key behaves the same way.

These were compared with Cloud Storage using AWS4 header and query signatures and V2
`AWS` headers (2026-11-09). On an object write
without an AWS signature, `x-amz-meta-*`, `x-amz-acl`, `x-amz-storage-class` and
`x-amz-metadata-directive` are accepted without effect, and `x-amz-copy-source` is
400 `InvalidArgument` with `Details` of `Missing copy source`. Other requests
without an AWS signature keep rejecting `x-amz-*` headers.

## Signed POST policy form uploads

`POST /BUCKET` with `multipart/form-data` accepts HTML form uploads (DESIGN-041,
EXT-BEH-004). Virtual-host routing uses `POST /` on the bucket host. Generate the
form with the official SDK helpers, for example Go
`storage.GenerateSignedPostPolicyV4(bucket, key, &storage.PostPolicyV4Options{...,
Insecure: true, Hostname: "127.0.0.1:4443", Style: storage.PathStyle()})` or Ruby
`bucket.generate_signed_post_policy_v4(key, issuer:, signing_key:)`. Post every
returned field, then the `file` part last. A policy signs the base64 policy
document, not the host, so the form can target the emulator's path-style URL
without re-signing.

Supported fields: `key` (including `${filename}`), `bucket`, `policy`,
V4 `x-goog-algorithm` (`GOOG4-RSA-SHA256` or `GOOG4-HMAC-SHA256`),
`x-goog-credential`, `x-goog-date`, `x-goog-signature`, V2 `GoogleAccessId` and
`signature`, `success_action_status`, `success_action_redirect`,
`Content-Type`, `Cache-Control`, `Content-Disposition`, `Content-Encoding`,
`Content-Length`, `x-goog-custom-time` and `x-goog-meta-*`. Field names are
case-insensitive. Other fields fail with 400 instead of being ignored: `acl`,
`Expires`, AWS form aliases and unknown names. V2 and V4 credentials cannot be
mixed or combined with an Authorization header. The file part's own Content-Type
is not stored; send a `Content-Type`
field.

The policy document must contain `expiration` (ISO 8601 basic or RFC 3339 with
any UTC offset) and `conditions`. Supported conditions are `{"field": "value"}`,
`["eq", "$field", "value"]`, `["starts-with", "$field", "prefix"]` and one
`["content-length-range", min, max]` (inclusive file size). A bucket condition is
required for V4 and binds the request bucket. V2 permits a policy without a
bucket condition; such a policy does not bind the bucket, but the resolved
principal must still have permission on the request bucket. Every V4 form field
except `policy` and `x-goog-signature` needs a condition; V2 exempts `policy`,
`GoogleAccessId` and `signature`. The file part is not a policy field. Every
condition must name a submitted field or the request bucket. The `key` condition
currently matches the submitted value before `${filename}`
substitution. Violations, expiry and malformed documents return 400
`InvalidPolicyDocument`; the file size outside the range returns 400
`EntityTooSmall` or `EntityTooLarge` and leaves no object.

In strict mode the registered signer's RSA public key or ACTIVE HMAC key verifies the signature
(403 `SignatureDoesNotMatch` or `InvalidAccessKeyId`), and the signer's IAM
permissions authorize the write: create for new objects, delete as well for
overwrites. V4 HMAC derives the signing key from the literal UTF-8 secret and the
four credential-scope components, then verifies HMAC-SHA256 over the exact
base64 policy field. V2 signs that exact field with RSA-SHA256 or legacy
HMAC-SHA1 and uses a base64 signature. RSA verification failure never retries
with HMAC. Every HMAC request resolves the current registered principal and key
state; key administration alone grants no object access.

A verified V4 policy currently gets a local signed URL validity window: it is
rejected more than 15 minutes before `x-goog-date`, and its `expiration` may be
at most 7 days after `x-goog-date` (400 `InvalidPolicyDocument`). These limits
have not been established as Google's form-policy contract; V2 checks expiration
only and does not inherit them. Valid signer
uploads work under public access prevention. A form
without `policy` or an `Authorization` header is anonymous and needs a public
create grant. As a local extension, a form without `policy` can instead use an
`Authorization` header and the authenticated principal's permissions; this is
not a verified Google form authentication contract. The permissive
profile ignores policy credentials as it does for signed URLs, but still
enforces the policy conditions and expiration. With `signed_urls=verify` it
verifies policy signatures and applies the `x-goog-date` window as strict mode
does, without checking the signer's permissions.

Uploads use the same streaming write path as XML PUT. Success returns 204 by
default, an empty 200, a 201 `PostResponse` XML document with `Location`,
`Bucket`, `Key` and `ETag`, or 303 to `success_action_redirect` (the redirect URL
is used unchanged). Fields before the file have no limit on their number or
individual size and share a total of 16 MiB, and custom metadata keeps its
documented 8 KiB. Exact provider error wording, redirect query parameters and
browser CORS for `POST /BUCKET` are not verified; see
`compatibility/features.json` and [the policy variant evidence](slices/xml-post-policy-variants.md).

## Verifying signed requests in permissive mode

By default, permissive mode accepts signed requests without checking their
signatures (`signed_urls=ignore`). `signed_urls=verify` (CLI `--signed-urls`,
environment `GCS_MIMIC_SIGNED_URLS`, JSON `signed_urls`) makes permissive mode
verify every XML request that carries signing query parameters, a signature
`Authorization` header or a signed POST policy exactly as strict mode does: V2
query and header signatures, V4 query and header RSA and HMAC signatures,
`AWS4-HMAC-SHA256`, signed `aws-chunked` uploads and V2/V4 POST policies with
their `x-goog-date` window. Failures get the strict XML errors, such as 403
`SignatureDoesNotMatch` for a changed request or header, 403
`InvalidAccessKeyId` for an unknown key and 400 `ExpiredToken` after expiry;
V2 expiry is checked too. An `InvalidAccessKeyId` message names where this
mode looks for keys, or says that the built-in test signer is disabled. The
ready event reports `"signed_urls"`.

Despite its name, the setting is not limited to signed URLs. It covers every
signature the XML API accepts, including `Authorization` header signatures
(`GOOG1`, `AWS`, `GOOG4-*` and `AWS4-HMAC-SHA256`) and POST policies. S3-style
clients that sign with made-up HMAC keys, which the default accepts, are
rejected with 403 `InvalidAccessKeyId` once `verify` is on; create HMAC keys
through the JSON API for them, or keep the default.

A verified signature then gets permissive access: no IAM or ACL check runs,
because permissive mode does not authorize unsigned requests either, and a
signed request must not be stricter than an unsigned one. A signer without
permission is therefore rejected only in strict mode. Unsigned requests and
JSON API routes are unchanged.

Signatures verify with these keys:

- the [built-in test signer](#built-in-test-signer), enabled by default when
  permissive mode verifies signatures;
- ACTIVE HMAC keys created through the JSON API. Their service account needs
  no identity entry, since nothing is authorized by identity;
- RSA public keys from an identity document, `identity_file` or the same
  registry inline in `identities`. Permissive mode accepts either only with
  `signed_urls=verify`, and its identities may declare
  only `id` and `public_keys` (and an empty or absent `tokens`).
  `project_roles`, `acl_entity`, non-empty `tokens` and
  `public_access_prevention_projects` require `auth_mode=strict` and fail
  startup, as does an identity without `public_keys`.

```sh
GCS_MIMIC_SIGNED_URLS=verify gcs-mimic serve --scheme http
gcs-mimic url sign --base-url http://localhost:4443 --bucket my-bucket \
  --object my-object --method GET --expires 15m
```

Strict mode always verifies: `signed_urls=verify` is accepted there, and
`signed_urls=ignore` with `auth_mode=strict` fails startup.
