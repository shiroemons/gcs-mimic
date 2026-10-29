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
as well for overwrites.
SHA256/Content-MD5 failures preserve the old object; sign headers to constrain
metadata, digest declarations and conditions. SHA256 checking is incremental;
the canonical URL payload remains UNSIGNED-PAYLOAD. CORS grants no permission.
The [trusted HTTPS Chromium journey](evidence/signed-browser.md) verifies actual
signed PUT/GET/HEAD, exposed headers, CORS denials, strict errors and restart.

Go storage v1.70.0 omits non-default ports from canonical Host. Prefer the helper
for local ports or expose a default-port endpoint. Ruby Storage 1.62.1 signs
`storage.googleapis.com` by default even with an emulator endpoint. Its pinned
test uses `file.signed_url(version: :v4, method: "GET"/"PUT", issuer:, signing_key:,
bucket_bound_hostname:)` with an explicit bucket/name File path because
bucket-bound signing removes the normal bucket prefix. This preserves host:port
before signing; see the [SDK details](../../sdk-tests/ruby/README.md) and
SPEC-GO-SIGNER-PORT / SPEC-RUBY-SIGNER-ENDPOINT. Strict verification is unchanged.
PAP suppresses stored `allUsers` and `allAuthenticatedUsers` grants in strict mode;
explicit signer grants remain usable. The journey compares the policy before,
during and after enforcement, shows public reactivation with `inherited`, then
enforces again for the restart check. A configured local parent fixture can keep
an inherited bucket effectively enforced; see the [PAP contract](slices/public-access-prevention.md).

Only `response-content-type` and `response-content-disposition` are supported.
Supply literal values through repeated `url sign --query NAME=VALUE`; the helper
encodes them before signing. These change response headers without mutating
stored metadata. Query changes invalidate the signature. Duplicate decoded names
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
`x-goog-algorithm` (`GOOG4-RSA-SHA256`), `x-goog-credential`, `x-goog-date`,
`x-goog-signature`, `success_action_status`, `success_action_redirect`,
`Content-Type`, `Cache-Control`, `Content-Disposition`, `Content-Encoding`,
`Content-Length`, `x-goog-custom-time` and `x-goog-meta-*`. Field names are
case-insensitive. Other fields fail with 400 instead of being ignored: `acl`,
`Expires`, V2 `GoogleAccessId`/`signature`, `GOOG4-HMAC-SHA256` and unknown
names. The file part's own Content-Type is not stored; send a `Content-Type`
field.

The policy document must contain `expiration` (ISO 8601 basic or RFC 3339 with
any UTC offset) and `conditions`. Supported conditions are `{"field": "value"}`,
`["eq", "$field", "value"]`, `["starts-with", "$field", "prefix"]` and one
`["content-length-range", min, max]` (inclusive file size). A bucket condition is
required and binds the request bucket. Every form field except `policy` and
`x-goog-signature` needs a condition, and every condition must name a submitted
field. The `key` condition matches the submitted value before `${filename}`
substitution. Violations, expiry and malformed documents return 400
`InvalidPolicyDocument`; the file size outside the range returns 400
`EntityTooSmall` or `EntityTooLarge` and leaves no object.

In strict mode the registered signer's RSA public key verifies the signature
(403 `SignatureDoesNotMatch` or `InvalidAccessKeyId`), and the signer's IAM
permissions authorize the write: create for new objects, delete as well for
overwrites. A verified policy also gets the signed URL validity window: it is
rejected more than 15 minutes before `x-goog-date`, and its `expiration` may be
at most 7 days after `x-goog-date` (400 `InvalidPolicyDocument`). Valid signer
uploads work under public access prevention. A form
without `policy` is anonymous and needs a public create grant. The permissive
profile ignores policy credentials as it does for signed URLs, but still
enforces the policy conditions and expiration.

Uploads use the same streaming write path as XML PUT. Success returns 204 by
default, an empty 200, a 201 `PostResponse` XML document with `Location`,
`Bucket`, `Key` and `ETag`, or 303 to `success_action_redirect` (the redirect URL
is used unchanged). Fields before the file are limited to 64 KiB each and
256 KiB in total. Exact provider error wording, redirect query parameters and
browser CORS for `POST /BUCKET` are not verified; see
`compatibility/features.json`.
