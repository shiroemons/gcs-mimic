# Build and run gcs-mimic

<!-- markdownlint-configure-file {"MD013": false} -->

The current slice supports JSON bucket create/get/list/delete and metadata PATCH/PUT, bucket IAM policy
get/set and object media/multipart upload,
metadata get/patch/update, generation-pinned copy, continuation rewrite, ordered compose,
prefix/delimiter/paged listing, whole-object and single-range download and delete,
with a persistent SQLite catalog and disk blobs.
Initial seed directory import runs before readiness. Path-style XML GET/HEAD and
bucket `allUsers` / `allAuthenticatedUsers` grants support public and private reads.
If-Range, transcoding, object IAM, testIamPermissions,
conditional policies, ACLs, virtual-host routing, XML writes/listing, signed URLs,
UBLA/PAP settings remain unavailable. JSON defaults and bucket-configured XML-read
CORS are available; see the browser journey below. HTTPS is the default listener mode;
HTTP examples explicitly select HTTP. TLS 1.2/1.3 uses built-in or supplied PEM files and
explicit client CA trust, as shown in the standalone HTTPS journey below.
The default permissive profile enforces no authentication; use an isolated local
development environment. An optional strict profile checks local bearer tokens
against project and bucket grants of predefined storage roles.
The [bucket contract](slices/json-bucket-crud.md) and
[object media](slices/json-object-media.md), [listing](slices/json-object-list.md),
[metadata](slices/json-object-metadata.md),
[multipart](slices/json-object-multipart.md) and
[strict authentication](slices/strict-auth.md) and
[bucket IAM](slices/bucket-iam.md), [bucket PATCH](slices/bucket-metadata-patch.md),
[bucket PUT](slices/bucket-metadata-put.md) and
[public object reads](slices/public-object-reads.md) contracts list supported fields and deviations.
The resumable journeys below specify the [frozen upload contract](slices/resumable-upload.md)
and its expected outputs; they await final-source execution and are not verification
evidence or a released-image claim. Implementation status remains in the
[feature ledger](../../compatibility/features.json).
Run the shell blocks in order in one Bash session from the repository root.

## Local HTTPS with an explicit development CA

HTTPS starts without certificate preparation. The binary embeds a development
certificate for `localhost`, `gcs`, `127.0.0.1` and `::1`; `tls ca` prints its
public CA without reading configuration or opening the data directory/network.
The shared development server key is deliberately public. Keep this emulator
isolated; custom certificate/key pairs remain available below. This standalone
journey uses Bash, curl and jq and changes no system trust store.

```sh
tls_work=$(mktemp -d)
cert_dir="$tls_work/certs"
mkdir "$cert_dir"
cargo build --locked
./target/debug/gcs-mimic tls ca > "$cert_dir/ca.pem"
./target/debug/gcs-mimic serve --listen 127.0.0.1:54443 \
  --api-base-url https://localhost:54443 --data-dir "$tls_work/data" \
  > "$tls_work/native.log" 2>&1 &
tls_pid=$!
for attempt in $(seq 1 50); do
  if ./target/debug/gcs-mimic healthcheck --url https://localhost:54443/_internal/ready; then break; fi
  sleep 0.1
done
bash scripts/tls-smoke.sh https://localhost:54443 "$cert_dir/ca.pem"
kill -TERM "$tls_pid"
wait "$tls_pid"
```

`--scheme https` is the default; omitting both TLS paths uses the built-in pair.
To use HTTP, explicitly select `--scheme http` and omit the TLS paths. Custom
TLS needs both `--tls-cert` and `--tls-key`; a single path fails. The supplied
PEM chain (leaf first) and private key are loaded and checked before recovery,
seeding, binding or readiness. Each is capped at 1 MiB. TLS 1.2/1.3 and HTTP/1.1
are supported; handshakes share `max_connections` and have a separate
`--tls-handshake-timeout-ms` deadline (default 5000, range 1–60000).
Configuration precedence is CLI > `GCS_MIMIC_` environment > JSON > defaults.
For example, use JSON `scheme`, `tls_cert`, `tls_key`, `tls_handshake_timeout_ms`
or environment `GCS_MIMIC_SCHEME`, `GCS_MIMIC_TLS_CERT`, `GCS_MIMIC_TLS_KEY`.
An omitted API base uses `https://127.0.0.1:4443`; set `--api-base-url` for a
different reachable host/port. Explicit HTTP(S) API bases remain independent for
reverse proxies. Neither `Host` nor forwarding headers change generated links.

Build and run the same journey with the non-root, read-only source image:

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-tls-demo-$$
export GCS_MIMIC_IMAGE=gcs-mimic:tls-dev
export GCS_MIMIC_PORT=54444
export GCS_MIMIC_API_BASE_URL=https://localhost:54444
docker build -t "$GCS_MIMIC_IMAGE" .
docker compose config --quiet
docker compose up -d --no-build --wait
bash scripts/tls-smoke.sh "$GCS_MIMIC_API_BASE_URL" "$cert_dir/ca.pem"
docker compose stop
docker compose start --wait
docker compose exec -T --interactive=false gcs \
  gcs-mimic healthcheck --url https://localhost:4443/_internal/ready
```

For peers that follow generated links, set `GCS_MIMIC_API_BASE_URL=https://gcs:4443`
and recreate the service before starting their upload. Host clients and browsers
use `https://localhost:54444`. Both names are in the certificate; a container's
`localhost` names that container. Mount only `ca.pem` in client containers and
pass it to their TLS client. The public URL helper accepts an independent base
and grants no access. Same-origin browser navigation works after explicitly
trusting the development CA in an isolated browser profile; cross-origin fetch
uses JSON defaults or the configured XML bucket rules. Never disable certificate
or hostname verification. The [TLS evidence](evidence/local-tls.md) records the
isolated Chromium trust procedure and failure checks.

The Docker image and `compose.yaml` default to built-in HTTPS without TLS mounts.
The built-in leaf expires on 2036-10-19 and its CA on 2036-10-20; renew the
embedded assets before then. To use a custom development CA instead, generate
throwaway two-day fixtures with OpenSSL and select the read-only TLS override:

```sh
custom_cert_dir="$tls_work/custom-certs"
bash scripts/tls-fixtures.sh "$custom_cert_dir"
export GCS_MIMIC_TLS_DIR="$custom_cert_dir"
docker compose -f compose.yaml -f compose.tls.yaml up -d --no-build --wait
bash scripts/tls-smoke.sh "$GCS_MIMIC_API_BASE_URL" "$custom_cert_dir/ca.pem"
```

Custom probes use `--ca-cert /tls/ca.pem`; the default probe trusts only the
built-in CA. Explicit `--ca-cert` replaces that trust rather than adding it.
`compose.http.yaml` explicitly selects HTTP for the detailed HTTP journey below.
Add `compose.strict.yaml` to require the existing local test identities.
Stopping/recreating preserves the named data volume. `docker compose ... down`
stops/removes this demo's containers without resetting its data. To reset only
this disposable project's local catalog, first stop it and explicitly remove
its named volume; never reset another project's data.

For official clients, run the [Go HTTPS suite](../../sdk-tests/go/README.md#https-with-an-explicit-development-ca)
and [Ruby HTTPS journey](../../sdk-tests/ruby/README.md) using the same CA and
advertised origin. Their endpoint guards prevent fallback to Google or metadata
servers. The Rust [wire tests](../../tests/tls.rs) cover untrusted CA, wrong
hostname, expiry, invalid files, stalled admission, strict grants and restart.
After all clients and containers stop, delete only `$tls_work`; it contains
this journey's disposable certificates and native data.

## Build and start with Compose

Install Git, Docker Engine/Desktop with Buildx and Compose v2 (`up --wait` and
`start --wait`), curl and jq. No Google credentials or cloud resources are needed.
Build from a checkout; no image is published. The commands use an isolated Compose
project and a dynamically allocated loopback port to avoid other local emulators.

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-object-demo-$$
export COMPOSE_FILE=compose.yaml:compose.http.yaml
export GCS_MIMIC_IMAGE="gcs-mimic:object-demo-$$"
docker build -t "$GCS_MIMIC_IMAGE" .
export GCS_MIMIC_PORT=0
docker compose config --quiet
docker compose up -d --wait
address=$(docker compose port gcs 4443)
endpoint="http://$address"
docker compose exec -T --interactive=false gcs gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
curl --disable --noproxy '*' --fail "$endpoint/_internal/health"
curl --disable --noproxy '*' --fail "$endpoint/_internal/ready"
```

The image runs as UID/GID 10001 on a read-only root. Compose mounts the named
`gcs-data` volume at `/var/lib/gcs-mimic`, prepared with writable ownership by the
image. SQLite, WAL, the ownership lock, staging and opaque blobs live there. Memory is limited to 128 MiB,
CPU to one, PIDs to 64 and file descriptors to 1024; these are configured bounds,
not measured resource requirements. The image also works with `docker run` when
an explicit writable volume is mounted at that path.

The default fixed `selfLink` origin is `http://127.0.0.1:4443`, independent of the
selected listener. With a dynamic port, set the actual advertised endpoint and
recreate the service; the named volume preserves its catalog:

```sh
export GCS_MIMIC_PORT=${address##*:}
export GCS_MIMIC_API_BASE_URL="$endpoint"
docker compose up -d --wait
```

Host clients use `$endpoint`. Peers on the Compose network use `http://gcs:4443`.
Inside a container, localhost refers to that container; `0.0.0.0` is a listener
address. `selfLink` always uses the one configured origin. Set a peer-reachable
origin instead if peers must follow those links. Host and forwarded headers are
never used to construct links. The `gcs-mimic url public` helper builds public
object URLs served by the path-style XML endpoint (see the strict section below).
Use the published host port for a host browser, and the service name for a peer
container consuming the URL itself. URL construction does not grant access;
browser cross-origin fetch uses JSON defaults or the XML CORS configuration below. The resumable
contract uses this same fixed `api_base_url` for session `Location` responses;
follow the host/peer configuration steps below before initiating a session.

## Create, inspect, list and delete buckets

```sh
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"demo-bucket","location":"EU","labels":{"team":"local"}}' \
  "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b?project=local-project&prefix=demo-&maxResults=1"
```

Create/get/list return 200 with JSON. Duplicate names return 409 across all local
projects. `metageneration` is the string `"1"`; timestamps and labels survive restart.
List is sorted and project/prefix scoped, with opaque continuation tokens. Limits
are 1000 for `noAcl` and 200 for `full`; `full` currently omits ACL/owner fields.
Only US/EU/ASIA locations and the documented storage classes are accepted.
Recognized unsupported settings return 501; invalid names/queries return 400.
Insert bodies are capped at 64 KiB; get/list/delete require no body.

Peer-container curl uses a digest-pinned, test-only official curl image:

```sh
peer_image=curlimages/curl:8.23.0@sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"peer-bucket"}' 'http://gcs:4443/storage/v1/b?project=local-project'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail http://gcs:4443/storage/v1/b/peer-bucket
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail 'http://gcs:4443/storage/v1/b?project=local-project&prefix=peer-'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -X DELETE http://gcs:4443/storage/v1/b/peer-bucket
```

## Patch bucket labels and default storage class

PATCH merges supplied label keys and preserves omitted keys. A key set to null
removes that label; `labels:{}` preserves the map and `labels:null` clears it.
The supported default class is metadata only; changing it does not rewrite
existing objects or simulate physical placement. This sequence uses the Compose
endpoint; the native smoke check below repeats the merge/removal steps.

```sh
patch_dir=$(mktemp -d)
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket" > "$patch_dir/before.json"
patch_revision=$(jq -er '.metageneration' "$patch_dir/before.json")
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"labels":{"stage":"dev"},"storageClass":"NEARLINE"}' \
  "$endpoint/storage/v1/b/demo-bucket?ifMetagenerationMatch=$patch_revision" > "$patch_dir/merged.json"
jq -e --argjson prior "$patch_revision" \
  '.labels == {"team":"local","stage":"dev"} and .storageClass == "NEARLINE" and (.metageneration | tonumber) == $prior + 1' \
  "$patch_dir/merged.json"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"labels":{"stage":null}}' "$endpoint/storage/v1/b/demo-bucket" > "$patch_dir/removed.json"
jq -e '.labels == {"team":"local"} and .storageClass == "NEARLINE"' "$patch_dir/removed.json"
patch_status=$(curl --disable --noproxy '*' --silent --show-error -o "$patch_dir/stale.json" \
  -w '%{http_code}' -X PATCH -H 'Content-Type: application/json' --data '{"labels":{"stage":"stale"}}' \
  "$endpoint/storage/v1/b/demo-bucket?ifMetagenerationMatch=$patch_revision")
test "$patch_status" = 412
jq -e '.error.errors[0].reason == "conditionNotMet"' "$patch_dir/stale.json"
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket" > "$patch_dir/after.json"
cmp "$patch_dir/removed.json" "$patch_dir/after.json"
rm -r "$patch_dir"
```

Each successful PATCH increments metageneration once and changes its ETag.
`updated` uses millisecond clock precision and can equal the preceding value.
`ifMetagenerationMatch` and `ifMetagenerationNotMatch` are checked atomically;
failed conditions leave state unchanged. Strict mode requires
`storage.buckets.update`. PATCH also accepts boolean `versioning.enabled` and
`defaultEventBasedHold`; changes affect future uploads while prior holds and
retained versions survive. Native null settings are rejected; legacy null
settings reset to false. See the [contract](slices/bucket-metadata-patch.md)
for profile, input and provider-parity limits. ACLs, nonempty lifecycle rules,
CORS, retention, predefined ACL queries and `userProject` return 501;
`projection=full` and `noAcl` return the same modeled subset without ACL/owner.

## Replace bucket labels and default storage class

PUT replaces the modeled metadata rather than merging it. Omitted or null labels
clear the map; omitted class becomes STANDARD and omitted versioning/default
hold settings become false. These defaults are explicit local choices pending
SPEC-BUCKET-PUT, not provider-verified defaults. Existing objects, retained
versions, holds, identity and IAM policy survive replacement. Changing a default
class does not rewrite existing objects or simulate physical placement.

Run this block after the create and PATCH blocks against the same `$endpoint`.
For a native server, set `endpoint` to its advertised local address and run those
same create, PATCH and PUT blocks in order.

```sh
put_dir=$(mktemp -d)
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket" > "$put_dir/before.json"
put_revision=$(jq -er '.metageneration' "$put_dir/before.json")
curl --disable --noproxy '*' --fail -X PUT -H 'Content-Type: application/json' \
  --data '{"labels":{"stage":"replacement"},"storageClass":"COLDLINE","versioning":{"enabled":true},"defaultEventBasedHold":true}' \
  "$endpoint/storage/v1/b/demo-bucket?ifMetagenerationMatch=$put_revision" > "$put_dir/replaced.json"
jq -e --argjson prior "$put_revision" \
  '.labels == {"stage":"replacement"} and .storageClass == "COLDLINE" and .versioning.enabled and .defaultEventBasedHold and (.metageneration | tonumber) == $prior + 1' \
  "$put_dir/replaced.json"
curl --disable --noproxy '*' --fail -X PUT -H 'Content-Type: application/json' \
  --data '{}' "$endpoint/storage/v1/b/demo-bucket" > "$put_dir/reset.json"
jq -e '(.labels // {}) == {} and .storageClass == "STANDARD" and (.versioning.enabled // false) == false and (.defaultEventBasedHold // false) == false' \
  "$put_dir/reset.json"
put_status=$(curl --disable --noproxy '*' --silent --show-error -o "$put_dir/stale.json" \
  -w '%{http_code}' -X PUT -H 'Content-Type: application/json' --data '{"labels":{"stage":"stale"}}' \
  "$endpoint/storage/v1/b/demo-bucket?ifMetagenerationMatch=$put_revision")
test "$put_status" = 412
jq -e '.error.errors[0].reason == "conditionNotMet"' "$put_dir/stale.json"
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket" > "$put_dir/after.json"
cmp "$put_dir/reset.json" "$put_dir/after.json"
rm -r "$put_dir"
```

Each successful PUT increments metageneration once and changes its ETag; failed
conditions leave the resource unchanged. Strict mode requires
`storage.buckets.update`. A supplied labels map replaces all keys, and per-key
null is invalid. Null/empty class is invalid in both profiles; native null
boolean settings are invalid, while legacy null settings become false.
Unsupported settings and ACL/billing queries return 501; projections expose
the modeled subset without ACL/owner. See the [contract](slices/bucket-metadata-put.md)
for accepted full-resource round trips and remaining provider questions.

The [final PUT verification](evidence/bucket-metadata-put.md) records execution
of the shared create/PATCH/PUT blocks against the native release binary and
locally built Docker image, including exact resource equality after restart.

## Verify bucket versioning and default holds

Create-time `versioning.enabled` retains overwritten or deleted generations.
`defaultEventBasedHold` protects newly uploaded bytes until explicitly released.
This native walkthrough also works with the compatibility profile; PATCH changes
supplied defaults and PUT replaces them, including resetting omitted settings.

```sh
history_dir=$(mktemp -d)
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"history-bucket","versioning":{"enabled":true},"defaultEventBasedHold":true}' \
  "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail --data-binary 'first version' \
  "$endpoint/upload/storage/v1/b/history-bucket/o?uploadType=media&name=example" \
  > "$history_dir/first.json"
first_generation=$(jq -er '.generation' "$history_dir/first.json")
test "$(jq -r '.eventBasedHold' "$history_dir/first.json")" = true
code=$(curl --disable --noproxy '*' --silent --show-error -X DELETE \
  -o "$history_dir/held-error.json" -w '%{http_code}' \
  "$endpoint/storage/v1/b/history-bucket/o/example")
test "$code" = 403
jq -e '.error.errors[0].reason == "objectUnderActiveHold"' "$history_dir/held-error.json"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"eventBasedHold":false}' "$endpoint/storage/v1/b/history-bucket/o/example"
curl --disable --noproxy '*' --fail --data-binary 'second version' \
  "$endpoint/upload/storage/v1/b/history-bucket/o?uploadType=media&name=example" \
  > "$history_dir/second.json"
second_generation=$(jq -er '.generation' "$history_dir/second.json")
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/history-bucket/o?versions=true" > "$history_dir/list.json"
jq -e '.items | length == 2' "$history_dir/list.json"
test "$(curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/history-bucket/o/example?generation=$first_generation&alt=media")" = 'first version'
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"eventBasedHold":false}' "$endpoint/storage/v1/b/history-bucket/o/example"
for generation in "$first_generation" "$second_generation"; do
  curl --disable --noproxy '*' --fail -X DELETE \
    "$endpoint/storage/v1/b/history-bucket/o/example?generation=$generation"
done
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/history-bucket"
rm -rf "$history_dir"
```

An ordinary delete in a versioned bucket archives the live version; it does not
empty the bucket. Delete every retained generation explicitly before deleting
that bucket. Generation-specific precondition wire parity remains an open
[contract question](../../compatibility/gaps.json).

## Upload, inspect, download and conditionally create an object

This exact payload includes its final newline; the supplied base64 MD5 covers all 22 bytes.
The server also calculates CRC32C. An incorrect declared checksum returns 400 and publishes
no replacement. Encode a slash in the object path as `%2F`; `%252F` denotes a literal `%2F`.

```sh
work_dir=$(mktemp -d)
printf 'Hello from gcs-mimic.\n' > "$work_dir/upload.txt"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' \
  -H 'x-goog-hash: md5=b9CmrqnrVfHIcCH+TLg1NA==' \
  --data-binary @"$work_dir/upload.txt" \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=notes%2Fhello.txt&ifGenerationMatch=0" \
  > "$work_dir/object.json"
jq . "$work_dir/object.json"
generation=$(jq -r .generation "$work_dir/object.json")
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt" | jq .
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media&generation=$generation" \
  --output "$work_dir/download.txt"
cmp "$work_dir/upload.txt" "$work_dir/download.txt"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/condition.json" \
  --write-out '%{http_code}' -H 'Content-Type: text/plain' \
  --data-binary @"$work_dir/upload.txt" \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=notes%2Fhello.txt&ifGenerationMatch=0")
test "$status" = 412
jq . "$work_dir/condition.json"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/not-modified" \
  --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifGenerationNotMatch=$generation")
test "$status" = 304
test ! -s "$work_dir/not-modified"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/nonempty.json" \
  --write-out '%{http_code}' -X DELETE "$endpoint/storage/v1/b/demo-bucket")
test "$status" = 409
jq . "$work_dir/nonempty.json"
```

Successful media upload returns 200 Object JSON. A failed create-if-absent returns 412;
a GET whose not-match condition is false returns 304 with an empty body. Mutations use
412 for that condition instead (local rule, SPEC-008). A nonempty bucket returns 409.
Ordinary `alt=media` responds directly with 200, a declared deviation from the documented
redirect flow. Metadata/media GET and DELETE accept Go SDK retry tokens without replay or
deduplication; repeated DELETE can therefore return 404 (SPEC-013). Media/multipart
upload tokens require `ifGenerationMatch`, including zero; without it they return
501 before body input. A committed conditioned retry returns 412; no response replay
is implemented. Empty/duplicate token headers return 400 `invalidParameter`.

Peer-container upload, metadata, download and delete use the same media subset:

```sh
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary 'peer data' \
  'http://gcs:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=peer%2Fhello.txt'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail 'http://gcs:4443/storage/v1/b/demo-bucket/o/peer%2Fhello.txt'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail 'http://gcs:4443/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media' \
  > "$work_dir/peer-download.txt"
cmp "$work_dir/upload.txt" "$work_dir/peer-download.txt"
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -X DELETE 'http://gcs:4443/storage/v1/b/demo-bucket/o/peer%2Fhello.txt'
```

## Copy an object with source and destination conditions

Copy runs on the server with a pinned source generation and bounded disk streaming.
An absent or JSON `null` body inherits supported editable source metadata, excluding
holds; a supplied resource replaces that metadata in the declared local profile.
Strict mode needs source `storage.objects.get`, destination `storage.objects.create`
and destination `storage.objects.delete` for an overwrite. The high-level Go
`CopierFrom` uses rewrite; this example exercises JSON `objects.copy` directly.

```sh
curl --disable --noproxy '*' --fail -X POST -H 'Content-Type: application/json' \
  --data 'null' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt/copyTo/b/demo-bucket/o/notes%2Fcopy.txt?sourceGeneration=$generation&ifSourceGenerationMatch=$generation&ifGenerationMatch=0" \
  > "$work_dir/copied.json"
jq -e --slurpfile source "$work_dir/object.json" \
  '.name == "notes/copy.txt" and .generation != $source[0].generation and .md5Hash == $source[0].md5Hash and .crc32c == $source[0].crc32c' \
  "$work_dir/copied.json"
copy_generation=$(jq -er .generation "$work_dir/copied.json")
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fcopy.txt?alt=media&generation=$copy_generation" \
  --output "$work_dir/copied.txt"
cmp "$work_dir/upload.txt" "$work_dir/copied.txt"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/copy-condition.json" \
  --write-out '%{http_code}' -X POST \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt/copyTo/b/demo-bucket/o/notes%2Fcopy.txt?ifGenerationMatch=0")
test "$status" = 412
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fcopy.txt?generation=$copy_generation"
```

The [copy contract](slices/object-copy.md) lists unsupported ACL/KMS/billing/retention
semantics and the open body/default/large-object parity questions. Copy, one/multi-call
rewrite and compose blocks pass against the native release and final Docker image;
the [shared final evidence](evidence/derived-objects.md) also records exact resource/byte
persistence through restart. These are local builds, not a published-image claim.

## Rewrite an object and follow continuation tokens

Rewrite selects source bytes on the server. This example uses the object and
generation from the upload block above and completes in one request:

```sh
curl --disable --noproxy '*' --fail -X POST \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt/rewriteTo/b/demo-bucket/o/notes%2Frewritten.txt?sourceGeneration=$generation&ifGenerationMatch=0" \
  > "$work_dir/rewrite-one.json"
jq -e '.kind == "storage#rewriteResponse" and .done == true and
  .objectSize == "22" and .totalBytesRewritten == "22" and
  .resource.name == "notes/rewritten.txt" and (has("rewriteToken") | not)' \
  "$work_dir/rewrite-one.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Frewritten.txt?alt=media" \
  --output "$work_dir/rewrite-one.txt"
cmp "$work_dir/upload.txt" "$work_dir/rewrite-one.txt"
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Frewritten.txt"
```

For a multi-call example, change storage class and request a 1 MiB maximum.
The maximum must be positive and divisible by 1048576; it only applies across
different locations or storage classes. Same-location/class rewrites complete
in one call. An absent or JSON `null` body inherits editable source metadata
except holds; `{}` replaces the modeled metadata with local omission defaults.
The `null` interpretation supports nil resources from official clients.

```sh
dd if=/dev/zero of="$work_dir/rewrite-source.bin" bs=1048576 count=3
curl --disable --noproxy '*' --fail -H 'Content-Type: application/octet-stream' \
  --data-binary @"$work_dir/rewrite-source.bin" \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=rewrite%2Fsource.bin&ifGenerationMatch=0" \
  > "$work_dir/rewrite-source.json"
rewrite_url="$endpoint/storage/v1/b/demo-bucket/o/rewrite%2Fsource.bin/rewriteTo/b/demo-bucket/o/rewrite%2Ffinished.bin?maxBytesRewrittenPerCall=1048576&ifGenerationMatch=0"
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"storageClass":"NEARLINE","metadata":{"workflow":"rewrite"}}' \
  "$rewrite_url" > "$work_dir/rewrite-progress.json"
jq -e '.done == false and .totalBytesRewritten == "1048576" and
  .objectSize == "3145728" and (has("resource") | not)' \
  "$work_dir/rewrite-progress.json"
while ! jq -e '.done == true' "$work_dir/rewrite-progress.json" >/dev/null; do
  rewrite_token=$(jq -er '.rewriteToken' "$work_dir/rewrite-progress.json")
  curl --disable --noproxy '*' --fail -X POST \
    "$rewrite_url&rewriteToken=$rewrite_token" > "$work_dir/rewrite-next.json"
  mv "$work_dir/rewrite-next.json" "$work_dir/rewrite-progress.json"
done
jq -e '.totalBytesRewritten == "3145728" and .resource.storageClass == "NEARLINE" and
  .resource.metadata.workflow == "rewrite" and (has("rewriteToken") | not)' \
  "$work_dir/rewrite-progress.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/rewrite%2Ffinished.bin?alt=media" \
  --output "$work_dir/rewrite-finished.bin"
cmp "$work_dir/rewrite-source.bin" "$work_dir/rewrite-finished.bin"
for rewrite_name in source finished; do
  curl --disable --noproxy '*' --fail -X DELETE \
    "$endpoint/storage/v1/b/demo-bucket/o/rewrite%2F$rewrite_name.bin"
done
```

Continuation may omit optional parameters/body; repeated values must match the
first request. The immediately previous token replays its response, including
the original completed generation, without another publication. Sessions expire
after a fixed week. At most 128 active sessions and 128 completed responses are
retained separately; older completion responses can be evicted before expiry.
Malformed/mismatched requests fail 400; unavailable, altered or expired keys fail
local 410. Changed unqualified sources fail 412; explicitly selected retained
generations can continue. The [contract](slices/object-rewrite.md) records these
local choices and provider questions. Strict calls require source read,
destination create and delete for overwrite; each call rechecks grants.
KMS/CSEK, ACL/owner, requester billing and context/retention remain unsupported.

## Select response fields and validate cached reads

Select from supported JSON response fields with comma lists, slash paths or
parentheses. Selection changes the response, preserving the complete stored
resource and its HTTP validator. Empty/whitespace selectors, wildcard children
and `fields` with `alt=media` return 501; unknown fields return 400. The
[fields contract](slices/json-response-fields.md) records the local limits.

```sh
curl --disable --noproxy '*' --fail --get \
  --data-urlencode 'fields=name,size' \
  --dump-header "$work_dir/etag-headers" \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt" \
  > "$work_dir/selected.json"
jq -e '. == {name:"notes/hello.txt",size:"22"}' "$work_dir/selected.json"
curl --disable --noproxy '*' --fail --get \
  --data-urlencode 'fields=items(name),nextPageToken' --data-urlencode 'maxResults=1' \
  "$endpoint/storage/v1/b/demo-bucket/o" > "$work_dir/selected-list.json"
jq -e '.items[0] == {name:"notes/hello.txt"} and (keys - ["items","nextPageToken"] | length == 0)' \
  "$work_dir/selected-list.json"
read_etag=$(awk 'tolower($1) == "etag:" {sub(/\r$/, "", $2); print $2}' "$work_dir/etag-headers")
test -n "$read_etag"
status=$(curl --disable --noproxy '*' --silent --show-error \
  --output "$work_dir/cached-body" --write-out '%{http_code}' \
  -H "If-None-Match: $read_etag" \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?fields=name")
test "$status" = 304
test ! -s "$work_dir/cached-body"
status=$(curl --disable --noproxy '*' --silent --show-error \
  --output "$work_dir/stale-etag.json" --write-out '%{http_code}' \
  -H 'If-Match: "stale"' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?fields=name")
test "$status" = 412
jq -e '.error.code == 412 and .error.errors[0].reason == "conditionNotMet"' \
  "$work_dir/stale-etag.json"
```

Use the response `ETag` header, including its quotes. Object metadata/media GET
and bucket/policy GET support these validators; the legacy-emulator bucket JSON `etag`
field can differ from its record-derived HTTP header. A metadata update or
overwrite changes the validator. If-Match uses strong comparison, If-None-Match
uses weak comparison, and conditions precede Range. Generation/metageneration
query conditions can combine with either header: Match query-condition or
If-Match failures return 412 before NotMatch query-condition or If-None-Match
304 responses. Write/list validators return 501.
Standalone date validators and If-Range also return 501; If-Modified-Since is
ignored with If-None-Match, and If-Unmodified-Since is ignored with If-Match.
See the [read validator contract](slices/json-read-etag-validators.md).

## Upload metadata and media in one multipart request

Write a `multipart/related` body with CRLF delimiters, JSON metadata first and media
second. Metadata contentType takes precedence over the media part header locally;
null or empty contentType falls back to that header, then `application/octet-stream`.
Custom metadata is limited to 8192 UTF-8 key/value bytes. The media cap excludes
framing, and each JSON part is limited to 65536 bytes.

```sh
printf 'Hello multipart.\n' > "$work_dir/multipart-media.txt"
printf '%s\r\n' \
  '--guide-boundary' \
  'Content-Type: application/json; charset=UTF-8' '' \
  '{"name":"multipart/hello.txt","contentType":"text/plain; charset=utf-8","metadata":{"team":"local","source":"multipart"}}' \
  '--guide-boundary' 'Content-Type: application/octet-stream' '' \
  > "$work_dir/upload.multipart"
tee -a "$work_dir/upload.multipart" < "$work_dir/multipart-media.txt" > /dev/null
printf '\r\n--guide-boundary--\r\n' >> "$work_dir/upload.multipart"
curl --disable --noproxy '*' --fail \
  -H 'Content-Type: multipart/related; boundary=guide-boundary' \
  --data-binary @"$work_dir/upload.multipart" \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=multipart&ifGenerationMatch=0" \
  > "$work_dir/multipart.json"
jq -e '.name == "multipart/hello.txt" and .contentType == "text/plain; charset=utf-8" and .metadata == {"team":"local","source":"multipart"}' \
  "$work_dir/multipart.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/multipart%2Fhello.txt" \
  > "$work_dir/multipart-get.json"
cmp "$work_dir/multipart.json" "$work_dir/multipart-get.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/multipart%2Fhello.txt?alt=media" \
  --output "$work_dir/multipart-download.txt"
cmp "$work_dir/multipart-media.txt" "$work_dir/multipart-download.txt"
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/multipart%2Fhello.txt"
```

An optional third JSON part may declare MD5/CRC32C; checksums, all framing and
transport EOF must pass before publication. Wrong checksums or malformed/truncated
containers preserve the previous object. The [contract](slices/json-object-multipart.md)
records MIME limits and local error choices. Resumable uploads use a separate
session protocol as described next.

## Resume and finalize an upload from the host

These are acceptance commands for the frozen contract, pending execution against
the final implementation. Continue in the same Bash session with `demo-bucket`,
`$work_dir` and the host-advertised `$endpoint` from above. Nonfinal chunks must
be multiples of 256 KiB; the last chunk may be smaller. This example sends
262144 zero bytes and a five-byte `done` line, for a total of 262149 bytes.

The session URI is a bearer capability: anyone holding it can write, query or
cancel this upload without another credential. Keep it and the response headers
private; disable shell tracing before capturing them. Initiation metadata and
bytes remain invisible to object GET/list until finalization. An incomplete
overwrite leaves the old object readable.

```sh
set +x
umask 077
dd if=/dev/zero of="$work_dir/resumable-first.bin" bs=262144 count=1
printf 'done\n' > "$work_dir/resumable-last.bin"
cp "$work_dir/resumable-first.bin" "$work_dir/resumable.bin"
tee -a "$work_dir/resumable.bin" < "$work_dir/resumable-last.bin" > /dev/null
test "$(wc -c < "$work_dir/resumable.bin")" -eq 262149
status=$(curl --disable --noproxy '*' --silent --show-error \
  --dump-header "$work_dir/resumable-init.headers" \
  --output "$work_dir/resumable-init.body" --write-out '%{http_code}' \
  -H 'Content-Type: application/json' \
  -H 'X-Upload-Content-Type: application/octet-stream' \
  -H 'X-Upload-Content-Length: 262149' \
  --data '{"metadata":{"source":"resumable-guide"}}' \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=resumable&name=resumable%2Fhello.bin&ifGenerationMatch=0")
test "$status" = 200
test ! -s "$work_dir/resumable-init.body"
tr -d '\r' < "$work_dir/resumable-init.headers" | grep -qi '^cache-control: no-store$'
session_uri=$(awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); print $2 }' \
  "$work_dir/resumable-init.headers")
[[ "$session_uri" == "$endpoint/"* ]]
status=$(curl --disable --noproxy '*' --silent --show-error --output /dev/null \
  --write-out '%{http_code}' "$endpoint/storage/v1/b/demo-bucket/o/resumable%2Fhello.bin")
test "$status" = 404
status=$(curl --disable --noproxy '*' --silent --show-error -X PUT \
  --dump-header "$work_dir/resumable-chunk.headers" \
  --output "$work_dir/resumable-chunk.body" --write-out '%{http_code}' \
  -H 'Content-Length: 262144' -H 'Content-Range: bytes 0-262143/262149' \
  --data-binary @"$work_dir/resumable-first.bin" "$session_uri")
test "$status" = 308
test ! -s "$work_dir/resumable-chunk.body"
tr -d '\r' < "$work_dir/resumable-chunk.headers" | grep -qi '^range: bytes=0-262143$'
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o?prefix=resumable%2F" | jq -e 'has("items") | not'
```

Query the persisted prefix with a zero-body PUT. A 308 response is upload
progress, not a redirect; do not use curl `--location`. Read the response Range
before resuming: its last byte plus one is the next offset. An empty prefix
has no Range header. `bytes */*` queries without declaring the total; `bytes */N`
can also finalize when exactly N bytes are already persisted, including N=0.

```sh
status=$(curl --disable --noproxy '*' --silent --show-error -X PUT \
  --dump-header "$work_dir/resumable-status.headers" \
  --output "$work_dir/resumable-status.body" --write-out '%{http_code}' \
  -H 'Content-Length: 0' -H 'Content-Range: bytes */262149' "$session_uri")
test "$status" = 308
test ! -s "$work_dir/resumable-status.body"
tr -d '\r' < "$work_dir/resumable-status.headers" | grep -qi '^range: bytes=0-262143$'
status=$(curl --disable --noproxy '*' --silent --show-error -X PUT \
  --output "$work_dir/resumable.json" --write-out '%{http_code}' \
  -H 'Content-Length: 5' -H 'Content-Range: bytes 262144-262148/262149' \
  --data-binary @"$work_dir/resumable-last.bin" "$session_uri")
test "$status" = 200
jq -e '.name == "resumable/hello.bin" and .size == "262149" and
  .metageneration == "1" and .metadata.source == "resumable-guide" and
  (.generation | test("^[1-9][0-9]*$")) and
  (.md5Hash | length > 0) and (.crc32c | length > 0)' "$work_dir/resumable.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/resumable%2Fhello.bin?alt=media" \
  --output "$work_dir/resumable-download.bin"
cmp "$work_dir/resumable.bin" "$work_dir/resumable-download.bin"
status=$(curl --disable --noproxy '*' --silent --show-error -X PUT \
  --output "$work_dir/resumable-completed.json" --write-out '%{http_code}' \
  -H 'Content-Length: 0' -H 'Content-Range: bytes */262149' "$session_uri")
test "$status" = 200
cmp "$work_dir/resumable.json" "$work_dir/resumable-completed.json"
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/resumable%2Fhello.bin"
```

Finalization returns 200 Object JSON and publishes one generation. Whole-object
MD5/CRC32C declarations are checked before publication; failed checksums,
preconditions or permission checks preserve an existing destination. Completed
status returns the original resource until the session's original deadline.
This session completion record does not implement invocation-token response replay.
For interrupted requests or retryable 408/429/5xx responses, query status or
resend the same chunk with bounded backoff; acknowledged bytes cannot be replaced.
Nonretryable checksum/permission errors require a new initiation.

### Cancel an incomplete session

Initiate another upload and cancel its capability URI, rather than deleting an
object. JSON resumable cancellation returns 499; omit curl `--fail` when checking
this expected response. A cancelled session returns local 410 before its original
deadline. At or after 604800 seconds from initiation it returns 404, as does an
unknown session. Activity never renews this seven-day lifetime. This guide does
not wait a week; expiry boundaries belong to injected-clock acceptance tests.

```sh
status=$(curl --disable --noproxy '*' --silent --show-error \
  --dump-header "$work_dir/resumable-cancel.headers" --output /dev/null \
  --write-out '%{http_code}' -X POST -H 'Content-Length: 0' \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=resumable&name=resumable%2Fcancel.bin&ifGenerationMatch=0")
test "$status" = 200
cancel_uri=$(awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); print $2 }' \
  "$work_dir/resumable-cancel.headers")
[[ "$cancel_uri" == "$endpoint/"* ]]
status=$(curl --disable --noproxy '*' --silent --show-error -X DELETE \
  -H 'Content-Length: 0' --output /dev/null --write-out '%{http_code}' "$cancel_uri")
test "$status" = 499
status=$(curl --disable --noproxy '*' --silent --show-error -X PUT \
  -H 'Content-Length: 0' -H 'Content-Range: bytes */*' \
  --output /dev/null --write-out '%{http_code}' "$cancel_uri")
test "$status" = 410
status=$(curl --disable --noproxy '*' --silent --show-error --output /dev/null \
  --write-out '%{http_code}' "$endpoint/storage/v1/b/demo-bucket/o/resumable%2Fcancel.bin")
test "$status" = 404
unset session_uri cancel_uri
```

### Follow Location from a Compose peer and choose a browser address

The existing Compose environment override sets the JSON configuration key
`api_base_url`; no additional Compose file is needed. Recreate the service with
`http://gcs:4443` advertised for a peer, preserving its data volume. The peer
must follow the returned Location unchanged from its own network. A host's
`127.0.0.1` would instead address the peer container itself.

```sh
export GCS_MIMIC_API_BASE_URL=http://gcs:4443
docker compose up -d --wait
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --silent --show-error --dump-header - \
  --output /dev/null --write-out '%{http_code}\n' \
  -H 'Content-Type: application/json' -H 'X-Upload-Content-Length: 9' \
  --data '{}' \
  'http://gcs:4443/upload/storage/v1/b/demo-bucket/o?uploadType=resumable&name=resumable%2Fpeer.txt&ifGenerationMatch=0' \
  > "$work_dir/resumable-peer.headers"
test "$(tail -n 1 "$work_dir/resumable-peer.headers")" = 200
peer_uri=$(awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); print $2 }' \
  "$work_dir/resumable-peer.headers")
[[ "$peer_uri" == http://gcs:4443/* ]]
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --silent --show-error -X PUT \
  --write-out '\n%{http_code}' \
  -H 'Content-Length: 9' -H 'Content-Range: bytes 0-8/9' \
  --data-binary 'peer data' "$peer_uri" > "$work_dir/resumable-peer.response"
test "$(tail -n 1 "$work_dir/resumable-peer.response")" = 200
sed '$d' "$work_dir/resumable-peer.response" | jq -e '.name == "resumable/peer.txt" and .size == "9"'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail \
  'http://gcs:4443/storage/v1/b/demo-bucket/o/resumable%2Fpeer.txt?alt=media' \
  > "$work_dir/resumable-peer.txt"
printf 'peer data' > "$work_dir/resumable-peer-expected.txt"
cmp "$work_dir/resumable-peer-expected.txt" "$work_dir/resumable-peer.txt"
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/resumable%2Fpeer.txt"
unset peer_uri
```

A host browser needs a host-reachable advertised address such as
`http://localhost:PORT`, using the published port, not `http://gcs:4443`. The next
commands check that advertised address using host curl. This selects an address
only: accepting `Origin` does not establish CORS/preflight support, and no browser
upload is claimed here. Cancel the address-check session and restore the host
configuration before continuing.

```sh
export GCS_MIMIC_API_BASE_URL="http://localhost:${GCS_MIMIC_PORT}"
docker compose up -d --wait
status=$(curl --disable --noproxy '*' --silent --show-error -X POST \
  -H 'Content-Length: 0' -H 'Origin: http://localhost:3000' \
  --dump-header "$work_dir/resumable-browser.headers" --output /dev/null \
  --write-out '%{http_code}' \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=resumable&name=resumable%2Fbrowser.bin&ifGenerationMatch=0")
test "$status" = 200
browser_uri=$(awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); print $2 }' \
  "$work_dir/resumable-browser.headers")
[[ "$browser_uri" == "$GCS_MIMIC_API_BASE_URL/"* ]]
status=$(curl --disable --noproxy '*' --silent --show-error -X DELETE \
  -H 'Content-Length: 0' --output /dev/null --write-out '%{http_code}' "$browser_uri")
test "$status" = 499
unset browser_uri
export GCS_MIMIC_API_BASE_URL="$endpoint"
docker compose up -d --wait
```

One fixed base cannot automatically serve both address spaces. Use separate
configurations as above or a common reachable DNS/proxy endpoint. A configured
proxy prefix is retained in Location; the proxy must strip it before forwarding.
Host, Forwarded and X-Forwarded-* never choose the advertised endpoint.

## Compose existing objects

Compose concatenates 1 to 32 sources in request order, all from one bucket and
storage class. Sources can themselves be composites. Cumulative component counts
are not limited to 1024; the reported count saturates at 2,147,483,647. A composite
has CRC32C and componentCount, and omits md5Hash. Set destination contentType
explicitly; the JSON adapter does not inherit a CLI's first-source MIME default.
The [compose contract](slices/object-compose.md) separates documented behavior
from local safety rules and unsupported ACL, encryption, billing and contexts.

Continue with the same permissive endpoint. These commands use a separate bucket
and remove it after verification.

```sh
compose_dir=$(mktemp -d)
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"compose-demo-bucket"}' \
  "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary '1234' \
  "$endpoint/upload/storage/v1/b/compose-demo-bucket/o?uploadType=media&name=first"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary '56789' \
  "$endpoint/upload/storage/v1/b/compose-demo-bucket/o?uploadType=media&name=second"
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"sourceObjects":[{"name":"first"},{"name":"second"}],"destination":{"contentType":"text/plain","metadata":{"workflow":"compose"}}}' \
  "$endpoint/storage/v1/b/compose-demo-bucket/o/composite/compose?ifGenerationMatch=0" \
  > "$compose_dir/object.json"
jq -e '.size == "9" and .componentCount == 2 and .crc32c == "4waSgw==" and (has("md5Hash") | not)' \
  "$compose_dir/object.json"
printf '123456789' > "$compose_dir/expected"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/compose-demo-bucket/o/composite?alt=media" > "$compose_dir/bytes"
cmp "$compose_dir/expected" "$compose_dir/bytes"
compose_status=$(curl --disable --noproxy '*' --silent --show-error \
  -o "$compose_dir/stale.json" -w '%{http_code}' -H 'Content-Type: application/json' \
  --data '{"sourceObjects":[{"name":"first"},{"name":"second"}]}' \
  "$endpoint/storage/v1/b/compose-demo-bucket/o/composite/compose?ifGenerationMatch=0")
test "$compose_status" = 412
jq -e '.error.errors[0].reason == "conditionNotMet"' "$compose_dir/stale.json"
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"sourceObjects":[{"name":"first"},{"name":"second"}],"deleteSourceObjects":true}' \
  "$endpoint/storage/v1/b/compose-demo-bucket/o/joined/compose?ifGenerationMatch=0&fields=name,componentCount,crc32c"
source_status=$(curl --disable --noproxy '*' --silent --show-error \
  -o "$compose_dir/missing.json" -w '%{http_code}' \
  "$endpoint/storage/v1/b/compose-demo-bucket/o/first")
test "$source_status" = 404
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/compose-demo-bucket/o/joined?alt=media" > "$compose_dir/joined"
cmp "$compose_dir/expected" "$compose_dir/joined"
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/compose-demo-bucket/o/composite"
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/compose-demo-bucket/o/joined"
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/compose-demo-bucket"
rm -rf "$compose_dir"
```

`deleteSourceObjects=true` permanently deletes only the pinned source generations,
including with bucket versioning enabled. The local implementation requires delete
permission in strict mode and rejects a held source before changing any object.
Duplicate entries contribute bytes twice and delete once; a source with the
destination name follows normal overwrite/versioning rules and is excluded from
source hard deletion. Publication and source removal
share one catalog transaction. Failed conditions, missing sources and invalid
requests leave the old destination and all source generations intact.

## List objects and conditionally change metadata

Create a second object so a one-entry page has a continuation token. Prefix and
delimiter group nested names into `prefixes`; `maxResults` counts items and prefixes.
Keep the same filters when continuing a page. Tokens observe committed live state,
not a snapshot spanning requests.

```sh
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data 'nested data' \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=notes%2Fnested%2Fother.txt"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o?prefix=notes%2F&delimiter=%2F" | jq .
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o?prefix=notes%2F&delimiter=%2F&maxResults=1" \
  > "$work_dir/page.json"
jq . "$work_dir/page.json"
page_token=$(jq -er .nextPageToken "$work_dir/page.json")
curl --disable --noproxy '*' --fail --get \
  --data-urlencode 'prefix=notes/' --data-urlencode 'delimiter=/' \
  --data-urlencode 'maxResults=1' --data-urlencode "pageToken=$page_token" \
  "$endpoint/storage/v1/b/demo-bucket/o" | jq .
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fnested%2Fother.txt"
metageneration=$(jq -r .metageneration "$work_dir/object.json")
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"contentType":"text/plain; charset=utf-8","cacheControl":"private, max-age=60","metadata":{"team":"local"}}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifMetagenerationMatch=$metageneration" \
  > "$work_dir/patched.json"
jq . "$work_dir/patched.json"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/stale.json" \
  --write-out '%{http_code}' -X PATCH -H 'Content-Type: application/json' \
  --data '{"metadata":{"stale":"rejected"}}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifMetagenerationMatch=$metageneration")
test "$status" = 412
jq . "$work_dir/stale.json"
metageneration=$(jq -r .metageneration "$work_dir/patched.json")
curl --disable --noproxy '*' --fail -X PUT -H 'Content-Type: application/json' \
  --data '{"contentType":"text/plain","metadata":{"only":"replacement"}}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifMetagenerationMatch=$metageneration" \
  > "$work_dir/object.json"
jq . "$work_dir/object.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o?prefix=notes%2F" > "$work_dir/list.json"
jq -e --slurpfile object "$work_dir/object.json" '.items == $object' "$work_dir/list.json"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media&generation=$generation" \
  --output "$work_dir/metadata-download.txt"
cmp "$work_dir/upload.txt" "$work_dir/metadata-download.txt"
```

PATCH preserves omitted fields and merges custom keys; null removes a key or field.
PUT replaces the supported writable fields, clearing omitted cache/language/encoding/
disposition values and custom keys. Both increment metageneration while preserving
generation, checksums and bytes; a stale precondition returns 412 without mutation.
Custom metadata is bounded to 8 KiB. ACL/IAM, advanced fields, partial response masks,
token replay and gzip transcoding remain unsupported. Stored cache directives round-trip
in metadata; nonpublic JSON media responses use `Cache-Control: no-cache`.

## Browser CORS and mobile application origins

JSON requests use default CORS response headers regardless of bucket settings.
Path-style XML object GET/HEAD uses the stored bucket rules. CORS only controls
browser visibility; strict-mode callers still need the operation's permission.
Run this block against the permissive `$endpoint` established above, or against
an isolated native HTTP listener. It creates and removes its own example bucket.

```sh
cors_dir=$(mktemp -d)
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"cors-guide-bucket"}' "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail --data-binary 'browser bytes' \
  "$endpoint/upload/storage/v1/b/cors-guide-bucket/o?uploadType=media&name=example"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"cors":[{"origin":["https://app.example:443","capacitor://localhost","ionic://localhost"],"method":["GET","HEAD"],"responseHeader":["ETag","X-Probe"],"maxAgeSeconds":0}]}' \
  "$endpoint/storage/v1/b/cors-guide-bucket?ifMetagenerationMatch=1" > "$cors_dir/bucket.json"
jq -e '.cors[0].origin[1] == "capacitor://localhost" and .metageneration == "2"' "$cors_dir/bucket.json"
curl --disable --noproxy '*' --fail -D "$cors_dir/preflight" -o /dev/null -X OPTIONS \
  -H 'Origin: https://app.example' -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: x-probe' "$endpoint/cors-guide-bucket/example"
tr -d '\r' < "$cors_dir/preflight" | grep -Fxi 'access-control-allow-origin: https://app.example'
curl --disable --noproxy '*' --fail -D "$cors_dir/actual" -o "$cors_dir/bytes" \
  -H 'Origin: capacitor://localhost' "$endpoint/cors-guide-bucket/example"
printf 'browser bytes' | cmp - "$cors_dir/bytes"
tr -d '\r' < "$cors_dir/actual" | grep -Fxi 'access-control-allow-origin: capacitor://localhost'
curl --disable --noproxy '*' --fail -D "$cors_dir/json" -o /dev/null \
  -H 'Origin: https://another.example' "$endpoint/storage/v1/b/cors-guide-bucket/o/example?alt=media"
tr -d '\r' < "$cors_dir/json" | grep -Fxi 'access-control-allow-origin: https://another.example'
curl --disable --noproxy '*' --fail -D "$cors_dir/denied" -o /dev/null \
  -H 'Origin: https://another.example' "$endpoint/cors-guide-bucket/example"
if grep -qi '^access-control-allow-origin:' "$cors_dir/denied"; then exit 1; fi
status=$(curl --disable --noproxy '*' --silent --show-error --output /dev/null \
  --write-out '%{http_code}' -X PATCH -H 'Content-Type: application/json' \
  --data '{"cors":[]}' "$endpoint/storage/v1/b/cors-guide-bucket?ifMetagenerationMatch=1")
test "$status" = 412
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/cors-guide-bucket/o/example"
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/cors-guide-bucket"
rm -r "$cors_dir"
```

An XML response without Allow-Origin is intentionally unreadable by a browser;
curl still sees the normal API response. `cors: []` or null clears rules,
PATCH omission preserves them, and PUT omission clears them. A missing max age
uses 3600 seconds. Whole-origin `*` and `scheme://*.domain` rules are supported.
HTTP port 80 and HTTPS port 443 match their omitted equivalents; custom schemes
have no inferred default port. XML never sends Allow-Credentials. Bearer headers
and browser `credentials: include` are different: the latter does not work for
XML, and credentialed JSON preflight is outside this scoped workflow.

For iOS hybrid apps, configure the Origin actually sent by the WebView, such as
`capacitor://localhost` or `ionic://localhost`; registering a native deep-link
scheme does not establish that Origin. The destination still uses the emulator's
reachable HTTP/HTTPS endpoint. Opaque origins use `null`, which is shared by many
documents and is not an app identity. The [mobile-origin research](slices/cors-mobile-origins.md)
separates Google-documented rules, local custom-scheme handling and unverified
Google/iOS behavior. Native HTTP bridges do not prove browser CORS enforcement.

For actual browser verification over trusted local HTTPS, use the isolated
[Chromium journey](evidence/cors-browser.md). It runs
`bash scripts/cors-browser.sh ENDPOINT BUCKET strict|permissive initial|restart`
after trusting the exported CA in that disposable browser environment. It
verifies JSON uploads/CRUD, ETag/Location, XML allow/deny, strict permission denial
and restart without disabling TLS verification. Configuration changes affect
new uncached requests; previous browser caches can persist until expiry.

## Stop, restart and deliberate reset

```sh
docker compose stop
docker compose start --wait
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media&generation=$generation" \
  --output "$work_dir/restarted.txt"
cmp "$work_dir/upload.txt" "$work_dir/restarted.txt"
docker compose down
docker compose up -d --wait
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt" > "$work_dir/restarted.json"
cmp "$work_dir/object.json" "$work_dir/restarted.json"
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifGenerationMatch=$generation"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt")
test "$status" = 404
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/demo-bucket"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/demo-bucket")
test "$status" = 404
```

Stop/start and ordinary down/up preserve the named volume, exact object bytes and metadata.
Object and empty-bucket delete return 204 with
an empty body; the subsequent get returns 404. No reset/admin route exists.
To demonstrate a deliberate reset, first create a disposable bucket:

```sh
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"reset-bucket"}' "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary 'reset data' \
  "$endpoint/upload/storage/v1/b/reset-bucket/o?uploadType=media&name=reset.txt"
docker compose down --volumes
docker compose up -d --wait
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/reset-bucket")
test "$status" = 404
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/reset-bucket/o/reset.txt?alt=media")
test "$status" = 404
```

`down --volumes` deletes **all data in this Compose project's managed volume**;
the final get returns 404. Use it only for a disposable local catalog. There is no
fixture replay in this unseeded journey; see the separate seed example below.
SIGINT/SIGTERM stop acceptance and drain; recovery and
exclusive data-directory ownership must finish before bind/readiness. A second
process using the same directory fails without reporting ready. Future-schema,
corrupt or inaccessible catalogs also prevent startup.

## Official Go SDK

Install Go (the harness downloads pinned Go 1.27.3), then run against the Compose
endpoint from the repository root:

```sh
(cd sdk-tests/go && GCS_MIMIC_ENDPOINT="$endpoint" GOTOOLCHAIN=auto go test -count=1 -v ./...)
```

The harness pins `cloud.google.com/go/storage` v1.70.0 and `google.golang.org/api`
v0.308.0. It sets `STORAGE_EMULATOR_HOST`, disables credentials, metrics, proxies
and redirects, and confines requests to the explicit emulator. It exercises
bucket CRUD/paging and BucketHandle.Update with SetLabel/DeleteLabel, default
class/settings, metageneration conditions and strict update denial, plus object
listing/Update/Attrs/NewReader/NewRangeReader/Delete, exact bytes/hashes, generation
conditions, encoded names and nonempty bucket errors. The harness uploads raw media with
its confined HTTP client. `TestObjectMultipartWriter` also verifies the high-level
Writer with `ChunkSize=0`, fixed/custom metadata, explicit hashes, create-if-absent
and conditioned overwrite, followed by Attrs/NewReader. The resumable acceptance
contract requires normal nonzero Writer chunking: the default is 16 MiB, and
256 KiB is suitable for a small multi-chunk example. Use data larger than the
selected ChunkSize to force the resumable transport; a small or empty Writer may
choose multipart instead. The pinned Go transport sends POST to the session URI
and can represent incomplete progress as HTTP 200 with
`X-HTTP-Status-Code-Override: 308`; terminal 200 contains Object JSON without that
override. Check `Write` and `Close` errors, keep automatic checksum validation,
and do not use `ChunkSize=0` as a resumable workaround. Writer context cancellation
does not cancel the server session; it remains until explicit cancellation or
expiry. Resumable SDK results await final-source verification. The harness creates and
cleans up its own random buckets. ACLs, token deduplication and unrestricted SDK support
are not claimed. Ruby/Rails remains unverified.
Default Go SDK XML reads now work for the path-style GET subset, including
selected generations, single ranges and generation/metageneration match headers;
JSON reads remain available through `storage.WithJSONReads()`. The strict tests
below cover anonymous public reads, registered-only grants and revocation.
For an isolated native server plus vet/test, use `bash scripts/sdk-go.sh`.

## Strict local authentication

`compose.strict.yaml` switches the same Compose project to `auth_mode=strict` and
mounts the local fixture registry `config/identities.json` read-only. Three
principals hold project-level roles on `local-project`; the bucket reader has no
project roles:

| Token | Role |
| --- | --- |
| `local-admin-token` | `roles/storage.admin` |
| `local-viewer-token` | `roles/storage.objectViewer` |
| `local-creator-token` | `roles/storage.objectCreator` |
| `local-bucket-reader-token` | None; receives a bucket-only grant below |

These tokens are deliberately public emulator fixtures, not Google credentials;
never reuse them for a real service. Pass both files to every command in this
section so the service keeps strict mode. Recreating the service keeps the port,
advertised endpoint and named volume selected above. The ready event reports
the mode and the number of registered identities:

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml up -d --wait
docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml logs --no-log-prefix gcs \
  | jq -ceR 'fromjson? | select(.event == "ready" and .auth_mode == "strict" and .identities == 4) | {auth_mode, identities}'
```

Anonymous requests receive 401 `required`; an unregistered bearer token receives
401 `authError` and never falls back to anonymous access:

```sh
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/anonymous.json" \
  --write-out '%{http_code}' "$endpoint/storage/v1/b?project=local-project")
test "$status" = 401
jq -e '.error.errors[0].reason == "required"' "$work_dir/anonymous.json"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/bad-token.json" \
  --write-out '%{http_code}' -H 'Authorization: Bearer not-a-registered-token' \
  "$endpoint/storage/v1/b?project=local-project")
test "$status" = 401
jq -e '.error.errors[0].reason == "authError"' "$work_dir/bad-token.json"
```

The admin creates a bucket, uploads a private object and reads it back:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data '{"name":"strict-bucket"}' \
  "$endpoint/storage/v1/b?project=local-project" | jq '{name}'
printf 'strict data\n' > "$work_dir/strict.txt"
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: text/plain' --data-binary @"$work_dir/strict.txt" \
  "$endpoint/upload/storage/v1/b/strict-bucket/o?uploadType=media&name=notes%2Fprivate%20file.txt" \
  > "$work_dir/strict-object.json"
jq '{name, generation}' "$work_dir/strict-object.json"
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media" \
  --output "$work_dir/strict-admin.txt"
cmp "$work_dir/strict.txt" "$work_dir/strict-admin.txt"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media")
test "$status" = 401
```

The object viewer can read and list objects. It cannot upload, and it cannot read
bucket metadata because `roles/storage.objectViewer` lacks `storage.buckets.get`.
A missing bucket returns 404 only to callers holding the permission in some
project; the viewer receives 403:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-viewer-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media" \
  --output "$work_dir/strict-viewer.txt"
cmp "$work_dir/strict.txt" "$work_dir/strict-viewer.txt"
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-viewer-token' \
  "$endpoint/storage/v1/b/strict-bucket/o" | jq '[.items[].name]'
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/viewer-upload.json" \
  --write-out '%{http_code}' -H 'Authorization: Bearer local-viewer-token' \
  -H 'Content-Type: text/plain' --data-binary 'viewer data' \
  "$endpoint/upload/storage/v1/b/strict-bucket/o?uploadType=media&name=notes%2Fviewer.txt")
test "$status" = 403
jq -e '.error.errors[0].reason == "forbidden"' "$work_dir/viewer-upload.json"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-viewer-token' "$endpoint/storage/v1/b/strict-bucket")
test "$status" = 403
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-viewer-token' "$endpoint/storage/v1/b/missing-strict-bucket")
test "$status" = 403
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-admin-token' "$endpoint/storage/v1/b/missing-strict-bucket")
test "$status" = 404
```

The object creator can create new objects but cannot overwrite a live object,
because an overwrite also needs `storage.objects.delete`. A failing precondition
is evaluated first and returns 412. The original object stays unchanged:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-creator-token' \
  -H 'Content-Type: text/plain' --data-binary 'created data' \
  "$endpoint/upload/storage/v1/b/strict-bucket/o?uploadType=media&name=notes%2Fcreated.txt" \
  | jq '{name}'
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/creator-overwrite.json" \
  --write-out '%{http_code}' -H 'Authorization: Bearer local-creator-token' \
  -H 'Content-Type: text/plain' --data-binary 'overwrite attempt' \
  "$endpoint/upload/storage/v1/b/strict-bucket/o?uploadType=media&name=notes%2Fprivate%20file.txt")
test "$status" = 403
jq -e '.error.errors[0].reason == "forbidden"' "$work_dir/creator-overwrite.json"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-creator-token' \
  -H 'Content-Type: text/plain' --data-binary 'overwrite attempt' \
  "$endpoint/upload/storage/v1/b/strict-bucket/o?uploadType=media&name=notes%2Fprivate%20file.txt&ifGenerationMatch=0")
test "$status" = 412
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-creator-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fcreated.txt")
test "$status" = 403
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media" \
  --output "$work_dir/strict-unchanged.txt"
cmp "$work_dir/strict.txt" "$work_dir/strict-unchanged.txt"
```

### Grant and revoke bucket-only access

Continue with the same strict server, `endpoint`, `work_dir`, `strict-bucket`
and private object from above. The admin reads the bucket policy and keeps its
opaque etag. PUT replaces the whole policy, so preserve existing bindings when
adding the reader:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/policy-before.json"
jq -e '.kind == "storage#policy" and .resourceId == "projects/_/buckets/strict-bucket"
  and (.etag | length > 0)' "$work_dir/policy-before.json"
jq -e '(.bindings | map(.members |= sort) | sort_by(.role)) == [
  {"role":"roles/storage.legacyBucketOwner",
   "members":["projectEditor:local-project","projectOwner:local-project"]},
  {"role":"roles/storage.legacyBucketReader","members":["projectViewer:local-project"]}
]' "$work_dir/policy-before.json"
jq '.bindings += [{"role":"roles/storage.objectViewer",
  "members":["user:local-bucket-reader@example.invalid"]}]' \
  "$work_dir/policy-before.json" > "$work_dir/policy-grant.json"
curl --disable --noproxy '*' --fail -X PUT -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/policy-grant.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/policy-granted.json"
jq -e --arg previous "$(jq -r .etag "$work_dir/policy-before.json")" \
  '.etag != $previous' "$work_dir/policy-granted.json"
```

New buckets include the documented convenience bindings for
`projectOwner:local-project` and `projectEditor:local-project` as
`roles/storage.legacyBucketOwner`, and `projectViewer:local-project` as
`roles/storage.legacyBucketReader`. The registry models predefined storage roles,
not basic project roles, so no local principal belongs to those convenience
groups. The admin's project grant continues to authorize policy changes.
See Google's [default bindings](https://docs.cloud.google.com/storage/docs/access-control/iam-roles#modifiable-behavior)
and [convenience values](https://docs.cloud.google.com/storage/docs/access-control/iam#convenience-values).

The new grant lets the reader download and list this bucket's objects:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-bucket-reader-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media" \
  --output "$work_dir/bucket-reader.txt"
cmp "$work_dir/strict.txt" "$work_dir/bucket-reader.txt"
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-bucket-reader-token' \
  "$endpoint/storage/v1/b/strict-bucket/o" \
  | jq -e 'any(.items[]; .name == "notes/private file.txt")'
```

It grants no access to other buckets, object creation or IAM policy management.
Create a second bucket to check the boundary, then remove it as the admin:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data '{"name":"strict-other-bucket"}' \
  "$endpoint/storage/v1/b?project=local-project" | jq '{name}'
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-bucket-reader-token' \
  "$endpoint/storage/v1/b/strict-other-bucket/o")
test "$status" = 403
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/reader-upload.json" --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-bucket-reader-token' --data-binary 'denied' \
  "$endpoint/upload/storage/v1/b/strict-bucket/o?uploadType=media&name=denied.txt")
test "$status" = 403
jq -e '.error.errors[0].reason == "forbidden"' "$work_dir/reader-upload.json"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-admin-token' "$endpoint/storage/v1/b/strict-bucket/o/denied.txt")
test "$status" = 404
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-bucket-reader-token' "$endpoint/storage/v1/b/strict-bucket/iam")
test "$status" = 403
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -X PUT -H 'Authorization: Bearer local-bucket-reader-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/policy-granted.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam")
test "$status" = 403
curl --disable --noproxy '*' --fail -X DELETE -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-other-bucket"
```

Submitting the old policy fails with 412 `conditionNotMet` and leaves the grant
intact. Fetch the current policy, remove only the reader member and submit its
current etag to revoke access:

```sh
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/policy-stale.json" \
  --write-out '%{http_code}' -X PUT -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/policy-before.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam")
test "$status" = 412
jq -e '.error.errors[0].reason == "conditionNotMet"' "$work_dir/policy-stale.json"
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/policy-current.json"
jq -e 'any(.bindings[]; .role == "roles/storage.objectViewer"
  and (.members | index("user:local-bucket-reader@example.invalid") != null))' \
  "$work_dir/policy-current.json"
jq '.bindings |= (map(.members -= ["user:local-bucket-reader@example.invalid"])
  | map(select(.members | length > 0)))' \
  "$work_dir/policy-current.json" > "$work_dir/policy-revoke.json"
curl --disable --noproxy '*' --fail -X PUT -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/policy-revoke.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/policy-revoked.json"
jq -e --arg previous "$(jq -r .etag "$work_dir/policy-current.json")" \
  '.etag != $previous' "$work_dir/policy-revoked.json"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-bucket-reader-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media")
test "$status" = 403
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  -H 'Authorization: Bearer local-bucket-reader-token' "$endpoint/storage/v1/b/strict-bucket/o")
test "$status" = 403
```

Always fetch a fresh policy before retrying a stale-etag update. Omitting `etag`
overwrites without a concurrency check. Both profiles persist validated policy
changes; permissive mode performs no permission checks. The JSON API
[get](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/getIamPolicy)
and [set](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/setIamPolicy)
references describe the policy fields and required permissions.

### Public and private object reads

`gcs-mimic url public` prints the XML-style path URL of an object. It reads no
configuration, environment, data directory or network and never changes a
permission. Every byte except RFC 3986 unreserved characters and `/` is
percent-encoded; if a segment is `.` or `..`, every `/` is encoded as `%2F`.
The private object uploaded above has no stored Cache-Control. Construct host
and peer URLs, then prove that anonymous reads are still denied before any grant:

```sh
public_url=$(docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml exec -T --interactive=false gcs \
  gcs-mimic url public --base-url "$endpoint" --bucket strict-bucket --object 'notes/private file.txt')
peer_public_url=$(docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml exec -T --interactive=false gcs \
  gcs-mimic url public --base-url http://gcs:4443 --bucket strict-bucket --object 'notes/private file.txt')
printf '%s\n' "$public_url"
test "$public_url" = "$endpoint/strict-bucket/notes/private%20file.txt"
test "$peer_public_url" = 'http://gcs:4443/strict-bucket/notes/private%20file.txt'
dot_url=$(docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml exec -T --interactive=false gcs \
  gcs-mimic url public --base-url "$endpoint" --bucket strict-bucket --object 'notes/../private file.txt')
test "$dot_url" = "$endpoint/strict-bucket/notes%2F..%2Fprivate%20file.txt"
```

```sh
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/private.xml" \
  --write-out '%{http_code}' "$public_url")
test "$status" = 401
grep -q '<Code>AuthenticationRequired</Code>' "$work_dir/private.xml"
```

```sh
status=$(docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' "$peer_public_url")
test "$status" = 401
```

Fetch a fresh policy and preserve its bindings and etag when granting anonymous
reads. `roles/storage.objectViewer` also allows object listing through the JSON
API; it grants access to every object in this bucket:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/public-before.json"
jq '.bindings += [{"role":"roles/storage.objectViewer","members":["allUsers"]}]' \
  "$work_dir/public-before.json" > "$work_dir/public-grant.json"
curl --disable --noproxy '*' --fail -X PUT -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/public-grant.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/public-granted.json"
strict_generation=$(jq -er .generation "$work_dir/strict-object.json")
status=$(curl --disable --noproxy '*' --silent --show-error \
  --dump-header "$work_dir/public-get.headers" --output "$work_dir/public.txt" \
  --write-out '%{http_code}' "$public_url?generation=$strict_generation")
test "$status" = 200
cmp "$work_dir/strict.txt" "$work_dir/public.txt"
tr -d '\r' < "$work_dir/public-get.headers" | grep -qi '^cache-control: public, max-age=3600$'
tr -d '\r' < "$work_dir/public-get.headers" | grep -qi "^x-goog-generation: $strict_generation$"
status=$(curl --disable --noproxy '*' --silent --show-error --head \
  --output "$work_dir/public-head.headers" --write-out '%{http_code}' \
  "$public_url?generation=$strict_generation")
test "$status" = 200
tr -d '\r' < "$work_dir/public-head.headers" | grep -qi '^cache-control: public, max-age=3600$'
tr -d '\r' < "$work_dir/public-head.headers" | grep -qi "^x-goog-generation: $strict_generation$"
```

The peer consumes its in-network URL, checking the same bytes, status and headers:

```sh
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --silent --show-error --fail --dump-header /dev/stderr \
  "$peer_public_url?generation=$strict_generation" \
  > "$work_dir/public-peer.txt" 2> "$work_dir/public-peer-get.headers"
cmp "$work_dir/strict.txt" "$work_dir/public-peer.txt"
grep -q '^HTTP/1.1 200' "$work_dir/public-peer-get.headers"
tr -d '\r' < "$work_dir/public-peer-get.headers" | grep -qi '^cache-control: public, max-age=3600$'
tr -d '\r' < "$work_dir/public-peer-get.headers" | grep -qi "^x-goog-generation: $strict_generation$"
status=$(docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --silent --show-error --head --output /dev/null \
  --dump-header /dev/stderr --write-out '%{http_code}' \
  "$peer_public_url?generation=$strict_generation" 2> "$work_dir/public-peer-head.headers")
test "$status" = 200
tr -d '\r' < "$work_dir/public-peer-head.headers" | grep -qi '^cache-control: public, max-age=3600$'
tr -d '\r' < "$work_dir/public-peer-head.headers" | grep -qi "^x-goog-generation: $strict_generation$"
```

Stop/start preserves the grant as well as the object; verify it before revoking:

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml stop
docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml start --wait
```

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/public-restarted.json"
cmp "$work_dir/public-granted.json" "$work_dir/public-restarted.json"
status=$(curl --disable --noproxy '*' --silent --show-error --output "$work_dir/public-restarted.txt" \
  --write-out '%{http_code}' "$public_url")
test "$status" = 200
cmp "$work_dir/strict.txt" "$work_dir/public-restarted.txt"
```

`allAuthenticatedUsers` matches any registered local identity, even one without
project or bucket roles; it does not allow anonymous access. Replace only the
public member, using the current etag. The bucket-reader token has no remaining
grant from the previous walkthrough:

```sh
jq '(.bindings[].members[] | select(. == "allUsers")) = "allAuthenticatedUsers"' \
  "$work_dir/public-restarted.json" > "$work_dir/authenticated-grant.json"
curl --disable --noproxy '*' --fail -X PUT -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/authenticated-grant.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/authenticated-granted.json"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/authenticated-anonymous.xml" \
  --write-out '%{http_code}' "$public_url")
test "$status" = 401
grep -q '<Code>AuthenticationRequired</Code>' "$work_dir/authenticated-anonymous.xml"
status=$(curl --disable --noproxy '*' --silent --show-error \
  -H 'Authorization: Bearer local-bucket-reader-token' --output "$work_dir/authenticated.txt" \
  --write-out '%{http_code}' "$public_url")
test "$status" = 200
cmp "$work_dir/strict.txt" "$work_dir/authenticated.txt"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/authenticated-invalid.xml" \
  -H 'Authorization: Bearer unregistered-token' --write-out '%{http_code}' "$public_url")
test "$status" = 400
grep -q '<Code>InvalidAuthentication</Code>' "$work_dir/authenticated-invalid.xml"
```

Fetch the current policy and remove the authenticated-only member to restore the
original private state. New anonymous requests fail with 401; the registered
bucket reader now receives 403:

```sh
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/public-current.json"
jq '.bindings |= (map(.members -= ["allAuthenticatedUsers"])
  | map(select(.members | length > 0)))' \
  "$work_dir/public-current.json" > "$work_dir/public-revoke.json"
curl --disable --noproxy '*' --fail -X PUT -H 'Authorization: Bearer local-admin-token' \
  -H 'Content-Type: application/json' --data-binary @"$work_dir/public-revoke.json" \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/policy-revoked.json"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' "$public_url")
test "$status" = 401
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/public-revoked.xml" \
  -H 'Authorization: Bearer local-bucket-reader-token' --write-out '%{http_code}' "$public_url")
test "$status" = 403
grep -q '<Code>AccessDenied</Code>' "$work_dir/public-revoked.xml"
```

```sh
status=$(docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' "$peer_public_url")
test "$status" = 401
```

Absent a stored Cache-Control value, public reads use `public, max-age=3600`;
private XML reads use `private, max-age=0`, and nonpublic JSON media uses `no-cache`.
Stored Cache-Control takes precedence on XML and publicly readable JSON media.
Revocation affects new requests; it cannot recall bytes already downloaded or
cached, and an already-open read may finish. XML GET supports one stored-byte
range (206/416); HEAD remains 200 for a satisfiable range with Content-Range.
Unsupported XML inputs/operations return 501. Strict signing attempts return
400 `InvalidAuthentication`; permissive signature query parameters remain
unsupported queries (501). ACLs,
UBLA/PAP settings and virtual-host routing remain outside this subset.

### Strict SDK tests and cleanup

The strict Go SDK tests use the same fixture tokens through an explicit endpoint
and a static token source, without Application Default Credentials or the
metadata server:

```sh
(cd sdk-tests/go && GCS_MIMIC_STRICT_ENDPOINT="$endpoint" GOTOOLCHAIN=auto \
  go test -count=1 -v -run '^(TestStrictAuth|TestXMLReadsStrict|TestXMLReadsEncodedNames|TestPublicReads)' ./...)
```

Stop/start with both files keeps strict mode and the data. Clean up as the admin,
then recreate the service from `compose.yaml` alone to return to the default
permissive profile:

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml stop
docker compose -f compose.yaml -f compose.http.yaml -f compose.strict.yaml start --wait
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/iam" > "$work_dir/policy-restarted.json"
jq -e --arg saved "$(jq -r .etag "$work_dir/policy-revoked.json")" \
  '.etag == $saved' "$work_dir/policy-restarted.json"
curl --disable --noproxy '*' --fail -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket/o/notes%2Fprivate%20file.txt?alt=media" \
  --output "$work_dir/strict-restarted.txt"
cmp "$work_dir/strict.txt" "$work_dir/strict-restarted.txt"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/strict-bucket")
test "$status" = 401
for name in notes%2Fprivate%20file.txt notes%2Fcreated.txt; do
  curl --disable --noproxy '*' --fail -X DELETE -H 'Authorization: Bearer local-admin-token' \
    "$endpoint/storage/v1/b/strict-bucket/o/$name"
done
curl --disable --noproxy '*' --fail -X DELETE -H 'Authorization: Bearer local-admin-token' \
  "$endpoint/storage/v1/b/strict-bucket"
docker compose up -d --wait
docker compose logs --no-log-prefix gcs \
  | jq -ceR 'fromjson? | select(.event == "ready" and .auth_mode == "permissive") | {auth_mode}'
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b?project=local-project" | jq .
```

Strict mode combines project grants with unconditional bucket IAM bindings from
the pinned predefined `roles/storage.*` snapshot on JSON bucket/object and XML read routes.
User and service-account members match registry IDs exactly. Conditions,
custom/basic roles and group/domain/federated/deleted principals are
unsupported (501); malformed members or unknown roles return 400. Object IAM,
`testIamPermissions`, ACLs, signed URLs, HMAC, XML writes/listing and UBLA/PAP
settings are unavailable. `allUsers` includes anonymous and registered callers;
`allAuthenticatedUsers` requires a registered identity. An invalid token never
falls back to anonymous access, even for a public object.
Unknown routes return 404 and `/_internal` probes stay unauthenticated. The
[strict authentication contract](slices/strict-auth.md) lists error bodies, check
order and the local rules recorded as SPEC-017.

Finish the Compose walkthrough by stopping and removing its containers and
network, disposable volume, temporary files and this walkthrough's exact image tag:

```sh
docker compose down --volumes
docker image rm "$GCS_MIMIC_IMAGE"
rm -r "$work_dir"
unset GCS_MIMIC_IMAGE GCS_MIMIC_PORT GCS_MIMIC_API_BASE_URL
```

## Native binary and configuration

Build with Rust 1.99.1 and the committed lockfile:

```sh
cargo +1.99.1 build --release --locked --bin gcs-mimic
native_dir=$(mktemp -d)
./target/release/gcs-mimic serve --scheme http --listen 127.0.0.1:0 --data-dir "$native_dir/data" \
  --api-base-url http://127.0.0.1:4443 > "$native_dir/server.log" 2>&1 &
native_pid=$!
trap 'kill -TERM "$native_pid" 2>/dev/null || true' EXIT
for attempt in $(seq 1 100); do
  if jq -e 'select(.event == "ready")' "$native_dir/server.log" >/dev/null; then break; fi
  kill -0 "$native_pid"
  sleep 0.1
done
native_endpoint="http://$(jq -r 'select(.event == "ready") | .listen' "$native_dir/server.log")"
./target/release/gcs-mimic healthcheck --url "$native_endpoint/_internal/ready"
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"native-bucket"}' "$native_endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary 'native data' \
  "$native_endpoint/upload/storage/v1/b/native-bucket/o?uploadType=media&name=hello.txt"
curl --disable --noproxy '*' --fail \
  "$native_endpoint/storage/v1/b/native-bucket/o/hello.txt?alt=media" > "$native_dir/download.txt"
printf 'native data' | cmp - "$native_dir/download.txt"
native_public_url=$(./target/release/gcs-mimic url public --base-url "$native_endpoint" \
  --bucket native-bucket --object hello.txt)
status=$(curl --disable --noproxy '*' --silent --show-error --output "$native_dir/xml.txt" \
  --write-out '%{http_code}' "$native_public_url")
test "$status" = 200
printf 'native data' | cmp - "$native_dir/xml.txt"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"labels":{"team":"local","stage":"dev"},"storageClass":"NEARLINE"}' \
  "$native_endpoint/storage/v1/b/native-bucket?ifMetagenerationMatch=1" > "$native_dir/patched.json"
jq -e '.labels == {"team":"local","stage":"dev"} and .storageClass == "NEARLINE" and .metageneration == "2"' \
  "$native_dir/patched.json"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"labels":{"stage":null}}' "$native_endpoint/storage/v1/b/native-bucket" > "$native_dir/removed.json"
jq -e '.labels == {"team":"local"} and .storageClass == "NEARLINE"' "$native_dir/removed.json"
curl --disable --noproxy '*' --fail "$native_endpoint/storage/v1/b/native-bucket" > "$native_dir/after.json"
cmp "$native_dir/removed.json" "$native_dir/after.json"
kill -TERM "$native_pid"
wait "$native_pid"
trap - EXIT
jq . "$native_dir/server.log"
rm -r "$native_dir"
```

Compose-only `GCS_MIMIC_IMAGE` and `GCS_MIMIC_PORT` must be unset before native startup:
the binary rejects unknown `GCS_MIMIC_` settings. The temporary native catalog above is
removed after a successful SIGTERM shutdown; use `--data-dir data` to keep a local catalog.
The native example uses permissive mode, so XML reads bypass authorization.
On macOS and other Unix systems outside Linux/Android, restarting on the same
port may wait for TIME_WAIT to drain (about 30 seconds on the verified macOS
host, at most 65 seconds). A `bind_wait` event explains the bounded wait;
an existing live listener still causes startup to fail. Linux/Android enables
address reuse while preserving live-listener exclusivity. Windows is unchanged.
For a wildcard listen address, the loopback probe may miss a live listener on
a specific non-loopback IP; startup then waits up to 65 seconds before failing
with address-in-use rather than taking over that listener's traffic.
SIGINT/SIGTERM cancel the bind wait and release the catalog promptly.
For private objects, start with `--auth-mode strict --identity-file config/identities.json`
and follow the public/private policy journey above using the native URL helper.
The resulting binary needs no Rust, Python, curl or Go installation. CLI overrides
`GCS_MIMIC_` environment, JSON config and defaults; `--config` overrides
`GCS_MIMIC_CONFIG`. Config files are capped at 64 KiB; invalid settings fail before
readiness. The current configuration is JSON; future TOML settings are unavailable.

| CLI option | Environment | Default | Range |
| --- | --- | --- | --- |
| `--listen` | `GCS_MIMIC_LISTEN` | `127.0.0.1:4443` | Numeric IP:port; port 0 allowed |
| `--scheme` | `GCS_MIMIC_SCHEME` | `https` | `https` or explicit `http`; one listener |
| `--tls-cert` | `GCS_MIMIC_TLS_CERT` | built-in development certificate | Optional PEM leaf-first chain, at most 1 MiB; requires matching key |
| `--tls-key` | `GCS_MIMIC_TLS_KEY` | built-in development key | Optional matching unencrypted PEM private key, at most 1 MiB; requires certificate |
| `--tls-handshake-timeout-ms` | `GCS_MIMIC_TLS_HANDSHAKE_TIMEOUT_MS` | 5000 | 1–60000; HTTPS only, within connection admission |
| `--data-dir` | `GCS_MIMIC_DATA_DIR` | `data` relative to cwd | Nonempty path; exclusive catalog ownership |
| `--seed-dir` | `GCS_MIMIC_SEED_DIR` | none | Stable read-only bucket-directory input; only before initial catalog completion |
| `--seed-project` | `GCS_MIMIC_SEED_PROJECT` | `local-project` | Existing bucket project validation; administrative import |
| `--api-base-url` | `GCS_MIMIC_API_BASE_URL` | `https://127.0.0.1:4443` | Default scheme follows listener mode; explicit absolute HTTP(S) base remains independent; optional base path, no credentials/query/fragment |
| `--max-connections` | `GCS_MIMIC_MAX_CONNECTIONS` | 32 | 1–1024 |
| `--worker-threads` | `GCS_MIMIC_WORKER_THREADS` | 2 | 1–32 |
| `--max-blocking-threads` | `GCS_MIMIC_MAX_BLOCKING_THREADS` | 2 | 1–32 |
| `--connection-timeout-ms` | `GCS_MIMIC_CONNECTION_TIMEOUT_MS` | 5000 | 1–60000 |
| `--object-request-timeout-ms` | `GCS_MIMIC_OBJECT_REQUEST_TIMEOUT_MS` | 900000 (15 minutes) | 1–86400000 (24 hours) |
| `--catalog-wait-timeout-ms` | `GCS_MIMIC_CATALOG_WAIT_TIMEOUT_MS` | 30000 | 1–60000 |
| `--shutdown-timeout-ms` | `GCS_MIMIC_SHUTDOWN_TIMEOUT_MS` | 5000 | 1–60000 |
| `--max-object-bytes` | `GCS_MIMIC_MAX_OBJECT_BYTES` | 1073741824 (1 GiB) | 1–5497558138880 (5 TiB) |
| `--auth-mode` | `GCS_MIMIC_AUTH_MODE` | `permissive` | `permissive` or `strict`; strict requires an identity file |
| `--compat-profile` | `GCS_MIMIC_COMPAT_PROFILE` | `native` | `native` or `legacy-emulator`; JSON key `compat_profile`; independent of authentication. The source bucket profile supplies `seed_project` when creation omits `project`; see the [contract](slices/bucket-core-parity.md). |
| `--identity-file` | `GCS_MIMIC_IDENTITY_FILE` | none | JSON identity registry, at most 64 KiB; only with `strict` |

The identity file is read once at startup; an invalid registry, `strict` without a
file or a file with `permissive` fails before readiness. Run
`gcs-mimic url public --help` for the URL helper, which accepts no serve options.

Each HTTP/1 connection handles one request, with a 16 KiB parser buffer, 64 headers
and a deadline. Object routes use a header deadline followed by renewed chunk-inactivity
deadlines; active transfers can exceed the default five seconds. After confirmed upload EOF,
body inactivity is suspended while storage finishes. Upload/media requests additionally
have an absolute 15-minute ceiling that chunks cannot renew. Expiry during upload body
streaming closes the connection without an HTTP error response or publication; expiry
during download truncates the stream after its headers and closes the connection.
For large/slow transfers, use `--object-request-timeout-ms 3600000` for one hour or set
the corresponding environment/JSON value. Advisory checks and post-EOF storage waits each default to 30 seconds,
capped by the remaining absolute request budget, and return 503 `backendError` on expiry.
After a post-EOF timeout the mutation outcome is unknown: the commit may still complete,
so re-read state or retry with generation/metageneration preconditions. Complete bodies
may also commit after disconnect or shutdown. SQLite lock waiting retains five seconds;
background filesystem work and shutdown catalog close have no separate execution bound.
Bucket/probe routes retain a total deadline.
Uploads have eight admission slots, a two-slot channel with at most 64 KiB
per copied chunk, and independent size checks before publication. Downloads use one channel
slot and a 64 KiB reader. No transaction or blocking worker waits for incoming body data.
These inspected bounds are not an RSS/cgroup/CPU benchmark; the 1 GiB default cap has not
been verified by a 1 GiB transfer under the Compose limit.
Unknown routes return 404 without body reads; probes retain
GET/HEAD support and body rejection. Catalog work has 32 admitted calls, one
executing blocking job, a 2 MiB SQLite cache and no memory map. The runtime bounds
blocking threads; queued/cancelled operations retain directory ownership until
execution finishes. Before confirmed EOF, a timeout/disconnect aborts the upload without
publishing partial bytes. Early rejections send a response and half-close the socket, then
discard at most 256 KiB for at most 100 ms. Generic admitted catalog writes and completed
upload bodies can still commit after the client disconnects; clients must read back state
when the response is lost. Trickle uploads cannot renew the absolute request ceiling;
cancelled uploads release slots after their cleanup finishes.

Schema-v1 bucket catalogs contain no objects and migrate transactionally through v3,
v4 and v5 to v6. Existing v3 objects gain default metadata columns; v5 adds the durable
seed initialization marker, and v6 adds default bucket IAM policies and an etag
counter. Migrated catalogs do not import fixtures. Unreleased schema-v2 catalogs
are rejected without changing their data; automatic recovery of deleted v2 generations is
unavailable. Recovery ignores `.DS_Store` and `._*` OS metadata; other invalid storage
entries stop startup with a path diagnostic. Object `selfLink` addresses the current
resource without a generation query; `mediaLink` selects the returned generation.

## Seed a fresh native catalog and read a byte range

After building the native binary above, run this standalone example in Bash from
the repository root. It uses fresh data and a separate seed directory.

```sh
unset GCS_MIMIC_IMAGE GCS_MIMIC_PORT GCS_MIMIC_API_BASE_URL
seed_demo=$(mktemp -d)
mkdir -p "$seed_demo/fixtures/example-bucket"
printf 'hello world' > "$seed_demo/fixtures/example-bucket/hello.txt"
./target/release/gcs-mimic serve --scheme http --listen 127.0.0.1:0 \
  --data-dir "$seed_demo/data" --seed-dir "$seed_demo/fixtures" \
  --seed-project local-project > "$seed_demo/server.log" 2>&1 &
seed_pid=$!
trap 'kill -TERM "$seed_pid" 2>/dev/null || true' EXIT
for attempt in $(seq 1 100); do
  if jq -e 'select(.event == "ready")' "$seed_demo/server.log" >/dev/null; then break; fi
  kill -0 "$seed_pid"
  sleep 0.1
done
seed_endpoint="http://$(jq -r 'select(.event == "ready") | .listen' "$seed_demo/server.log")"
./target/release/gcs-mimic healthcheck --url "$seed_endpoint/_internal/ready"
curl --disable --noproxy '*' --fail \
  "$seed_endpoint/storage/v1/b/example-bucket/o/hello.txt?alt=media" > "$seed_demo/full.txt"
printf 'hello world' | cmp - "$seed_demo/full.txt"
curl --disable --noproxy '*' --fail-with-body -sS -D "$seed_demo/range.headers" \
  -H 'Range: bytes=0-4' \
  "$seed_endpoint/download/storage/v1/b/example-bucket/o/hello.txt?alt=media" > "$seed_demo/range.txt"
printf 'hello' | cmp - "$seed_demo/range.txt"
sed -n '1,20p' "$seed_demo/range.headers"
printf 'Range body: '; sed -n '1p' "$seed_demo/range.txt"; printf '\n'
kill -TERM "$seed_pid"
wait "$seed_pid"
trap - EXIT
jq . "$seed_demo/server.log"
rm -r "$seed_demo"
```

Expect 206, `Content-Range: bytes 0-4/11`, `Content-Length: 5` and `hello`.
Closed/open/suffix ranges use stored bytes; pin `generation` when assembling
multiple requests. Unsatisfiable ranges return 416. If-Range and required gzip
decompression return 501. See the [range contract](slices/json-object-range.md)
for local malformed/multiple-range fallback rules and header-parity limits.

Each top-level seed directory names a bucket; nested regular files name objects
with `/` separators. Empty bucket directories work. Imported objects have explicit
`application/octet-stream` MIME; size, MD5 and CRC32C come from their bytes.
Keep fixtures stable and read-only throughout startup. Symlinks, special files,
invalid names and overlapping roots fail before readiness. Traversal is limited
to 10000 entries and 32 nested directories; `max_object_bytes` applies per new file.
No sidecar metadata is imported.

A durable seed marker prevents replay after restart or deletion of all buckets.
Interrupted imports resume create-if-absent and preserve existing objects.
First startup without `seed_dir` and migrations of existing catalogs also mark
initialization complete: configuring fixtures later does not import them. Use a
new data directory or deliberately reset a disposable volume to seed again.
Import is administrative in both profiles; strict API reads still need a
permitted local bearer token. See the [seed contract](slices/seed.md).

### Seed with Compose

Use a separate project with a new volume. Build its image from this checkout;
the seed source is the checked-in `fixtures/local-bucket/hello.txt`.

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-seed-demo-$$
export GCS_MIMIC_IMAGE="gcs-mimic:seed-demo-$$"
docker build -t "$GCS_MIMIC_IMAGE" .
export GCS_MIMIC_PORT=0
docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml up -d --wait
seed_address=$(docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml port gcs 4443)
curl --disable --noproxy '*' --fail "http://$seed_address/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail "http://$seed_address/storage/v1/b/local-bucket/o"
curl --disable --noproxy '*' --fail \
  "http://$seed_address/storage/v1/b/local-bucket/o/hello.txt?alt=media"
docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml stop
```

The last response is `Hello from the emulator.` followed by a newline. The override
mounts fixtures read-only. Keep both `-f` arguments on restart/recreate commands;
add `-f compose.strict.yaml` and the local admin bearer token for strict mode.
Ordinary `stop` or `down` preserves data. To delete only this disposable demo's
emulator data, explicitly run `docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml
down --volumes`; the next `up -d --wait` imports into the fresh volume.
This complete guide, including bucket versioning/default holds, resumable upload,
seeded Compose and strict bucket IAM/public/private journeys, passed on 2026-10-22;
see the [final bucket integration evidence](evidence/bucket-core-parity.md).

## Verification and troubleshooting

```text
cargo +1.99.1 test --workspace --locked
cargo +1.99.1 fmt --all --check
cargo +1.99.1 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.99.1 xtask compatibility check
bash scripts/smoke-containers.sh
```

The smoke script builds arm64/amd64 images, checks isolated probes, host and peer
curl CRUD, stop/start and down/up persistence, deliberate reset, resource settings
and both shutdown signals. It removes only its own test volumes and containers.
On Apple Silicon, arm64 is native and amd64 uses emulation. Native amd64, remote
CI, real-cloud conformance and performance measurements remain unverified.

For connection errors, inspect `docker compose ps`, `docker compose logs gcs`,
port mapping and the caller's network. For startup errors, check catalog path
ownership, disk space, another owner process and config overrides. Unknown routes return
404; recognized unsupported object/bucket semantics return 501 rather than false success.
If initiation succeeds but the returned Location is unreachable, check
`GCS_MIMIC_API_BASE_URL` / `--api-base-url` / JSON `api_base_url` and recreate the
service with an origin reachable by the client following that URI. Host clients
need the published host port; peers need `gcs:4443`; browsers need their own
reachable hostname. Changing the listener, Host/forwarding headers or public URL
helper does not fix an advertised base. Start a new session after correcting it,
and do not expose the old capability URI in diagnostic logs.
In strict JSON responses, 401 `required` means an anonymous request lacks a
matching grant, 401
`authError` means the credential is unregistered or unsupported (including
`access_token` or signature query parameters), and 403 `forbidden` means the
principal lacks the permission through the effective project or bucket grants.
XML uses 400 `InvalidAuthentication` for invalid or unsupported credentials,
401 `AuthenticationRequired` for an anonymous denial and 403 `AccessDenied`
for an authenticated permission denial. Check the ready event's
`auth_mode`: running `docker compose up` without `compose.strict.yaml` recreates the
service in permissive mode.
Missing/truncated referenced blobs prevent readiness; host power loss and every individual
fsync crash window remain unverified. See the [integration evidence](evidence/object-media.md),
[multipart integration evidence](evidence/multipart.md),
[strict authentication evidence](evidence/strict-auth.md),
[public-read integration evidence](evidence/public-object-reads.md) and the
[ledger](../../compatibility/features.json) for exact verification scope.
