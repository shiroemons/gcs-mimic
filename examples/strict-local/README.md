# Production-like local access

Paste one block into your application's `compose.yaml` to get Cloud Storage access that behaves like production during local development and tests:

- Private buckets stay private: a browser reaches them only through signed URLs.
- Public buckets are readable only through their `allUsers` grant.
- Your application's servers need no code change: Go storage with `STORAGE_EMULATOR_HOST` sends no credentials and still gets in.
- URLs that Go storage signs for a local port verify.
- Rails Active Storage works with the same signer.

This recipe needs a gcs-mimic release newer than 0.13.0. 0.13.0 stops at startup with `Unknown setting` for `GCS_MIMIC_TEST_SIGNER`, `GCS_MIMIC_SIGNED_HOST_PORT`, `GCS_MIMIC_TRUSTED_LISTEN` and `GCS_MIMIC_TRUSTED_PRINCIPAL`. Until such a release is out, build an image from a checkout with `docker build -t gcs-mimic:local .` and set `GCS_MIMIC_IMAGE=gcs-mimic:local`. Inline `configs` need Docker Compose 2.23.1 or later.

## Add it to your compose.yaml

Add the `gcs` service under your `services:`, and the `configs:` and `volumes:` entries under those top-level keys, creating them if your file has none. The block is [compose.yaml](compose.yaml) in this directory, unchanged; `scripts/check-docs.sh` keeps the two identical, and the container smoke test runs that file.

<!-- strict-local compose.yaml -->
```yaml
services:
  # Production-like local Cloud Storage from gcs-mimic's examples/strict-local recipe.
  # Local development and tests only: the built-in test signer's private key is public.
  gcs:
    image: ${GCS_MIMIC_IMAGE:-ghcr.io/shiroemons/gcs-mimic:0.13.0}
    ports:
      # Browsers use the main listener. Never publish the trusted port 4444:
      # anything that reaches it acts as app@example.invalid without credentials.
      - "127.0.0.1:${GCS_MIMIC_PORT:-4443}:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_AUTH_MODE: strict
      GCS_MIMIC_IDENTITY_FILE: /etc/gcs-mimic/identities.json
      GCS_MIMIC_BUCKETS_FILE: /etc/gcs-mimic/buckets.json
      # Apply edits of gcs_buckets to buckets that already exist in the volume.
      GCS_MIMIC_BUCKETS_FILE_MODE: reconcile
      GCS_MIMIC_TEST_SIGNER: enabled
      # Go storage drops the port from the host it signs.
      GCS_MIMIC_SIGNED_HOST_PORT: lenient
      # Application servers on the Compose network use gcs:4444 without credentials.
      GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444
      GCS_MIMIC_TRUSTED_PRINCIPAL: app@example.invalid
    configs:
      - source: gcs_identities
        target: /etc/gcs-mimic/identities.json
      - source: gcs_buckets
        target: /etc/gcs-mimic/buckets.json
    volumes:
      - gcs-data:/var/lib/gcs-mimic
    # No read_only: Compose copies inline configs into the container and refuses a read-only one.
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    healthcheck:
      # Probes the main listener, as the image's own healthcheck does.
      test: [CMD, gcs-mimic, healthcheck]
      interval: 2s
      timeout: 2s
      start_period: 2s
      retries: 15
configs:
  gcs_identities:
    content: |
      {"identities": [
        {"id": "app@example.invalid", "tokens": [],
         "project_roles": {"local-project": ["roles/storage.admin"]}},
        {"id": "test-signer@gcs-mimic.invalid", "tokens": [],
         "project_roles": {"local-project": ["roles/storage.objectUser"]}}
      ]}
  gcs_buckets:
    content: |
      {"version": 1, "project": "local-project", "buckets": [
        {"name": "app-public",
         "iamPolicy": {"bindings": [{"role": "roles/storage.objectViewer", "members": ["allUsers"]}]},
         "cors": [{"origin": ["http://localhost:${FRONTEND_PORT:-3000}"], "method": ["GET", "HEAD"],
                   "responseHeader": ["Content-Type"], "maxAgeSeconds": 3600}]},
        {"name": "app-private",
         "cors": [{"origin": ["http://localhost:${FRONTEND_PORT:-3000}"], "method": ["PUT"],
                   "responseHeader": ["Content-Type", "Content-MD5", "Content-Disposition",
                                      "Cache-Control", "x-goog-content-length-range"],
                   "maxAgeSeconds": 3600}]}
      ]}
volumes:
  gcs-data:
```

Each application server that uses Cloud Storage needs these lines. `GCS_BROWSER_HOST` is your application's own setting, read by the signing code below, not by gcs-mimic or Go storage:

```yaml
services:
  api:
    environment:
      STORAGE_EMULATOR_HOST: gcs:4444
      GCS_BROWSER_HOST: localhost:${GCS_MIMIC_PORT:-4443}
    depends_on:
      gcs:
        condition: service_healthy
```

Set `FRONTEND_PORT` when your frontend does not run on port 3000, and `GCS_MIMIC_PORT` when port 4443 is taken, then run `docker compose up -d --wait`.

## What each part does

| Part | Why |
| --- | --- |
| `GCS_MIMIC_SCHEME: http` | Plain HTTP, as fake-gcs-server serves it; no client needs a CA. |
| `GCS_MIMIC_AUTH_MODE: strict` | Every request is authorized by IAM and ACLs. A request without credentials is anonymous and gets only `allUsers` grants. |
| `GCS_MIMIC_IDENTITY_FILE` and `gcs_identities` | Registers the two identities below and their roles on `local-project`. Neither has a token: the application never presents one, and the test signer authenticates with signatures. |
| `GCS_MIMIC_BUCKETS_FILE` and `gcs_buckets` | Creates the buckets at startup with their policy and CORS rules. |
| `GCS_MIMIC_BUCKETS_FILE_MODE: reconcile` | Writes the fields an entry sets to a bucket that already exists in the volume, so edits of `gcs_buckets` apply; see [Change the buckets](#change-the-buckets). |
| `GCS_MIMIC_TEST_SIGNER: enabled` | Turns on the built-in signer `test-signer@gcs-mimic.invalid`, whose private key ships with gcs-mimic, so nothing has to generate or register a key. Strict mode leaves it off by default and warns at startup when it is on. |
| `GCS_MIMIC_SIGNED_HOST_PORT: lenient` | Go storage leaves the port out of the host it signs, so its URLs for `localhost:4443` would fail with 403 `SignatureDoesNotMatch`. Lenient checks such a V4 signature once more without the port; the host name, path, query, signed headers and expiry stay bound. |
| `GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444` | A second, plain HTTP listener for your servers on the Compose network. It binds every interface inside the container so that other containers reach it; leaving it out of `ports:` keeps it off your machine's network. |
| `GCS_MIMIC_TRUSTED_PRINCIPAL: app@example.invalid` | A request without credentials on the trusted listener acts as this identity, under the same IAM and ACL checks as a request with its token. |
| `ports` | Publishes only the main listener, and only on loopback. |
| `gcs-data` volume | Keeps buckets and objects across `docker compose down` and `up`. |
| `healthcheck` | Lets services wait with `condition: service_healthy`. |
| No `read_only` | Compose refuses to create inline configs in a read-only container. |

The buckets:

| Bucket | Access | CORS for `http://localhost:${FRONTEND_PORT:-3000}` |
| --- | --- | --- |
| `app-public` | Its policy grants `allUsers` `roles/storage.objectViewer`, so anyone reads its objects without credentials. The policy replaces the default one, as `setIamPolicy` does. | `GET` and `HEAD`, so a page can `fetch()` its objects. `<img>` and other plain embeds need no CORS. |
| `app-private` | No `iamPolicy`, so it keeps the default policy, which grants nothing to `allUsers`; the two identities reach it through their project roles. | `PUT` for uploads through signed URLs. A browser lists every header it sets beyond the CORS-safelisted ones in its preflight, and the bucket must allow each: `Content-Type` for any upload with a type, the others because Rails direct uploads send them. Add `GET` if pages `fetch()` signed downloads. |

### Roles

`app@example.invalid` has `roles/storage.admin`, so your servers and test setup can do what a project owner's service account does: create buckets, change policies, read and write every object. If your servers only read and write objects, narrow it to `roles/storage.objectUser`, so that a missing permission fails locally as it would in production.

`test-signer@gcs-mimic.invalid` has `roles/storage.objectUser`. A signed download needs `storage.objects.get`; a signed upload needs `storage.objects.create`, and `storage.objects.delete` as well when it replaces an object. Rails also deletes (`purge`), lists (`delete_prefixed`) and updates metadata through the same identity. `roles/storage.objectUser` is the smallest predefined role with all of these; `roles/storage.objectViewer` plus `roles/storage.objectCreator` lack delete and update. It grants no bucket or IAM administration, which matters because anyone on your machine can sign as this identity.

## Connect each client

| Client | Address | Credentials |
| --- | --- | --- |
| Your servers on the Compose network | `gcs:4444` (trusted listener) | None; they act as `app@example.invalid` |
| Browser | `http://localhost:${GCS_MIMIC_PORT:-4443}` (main listener) | None for `app-public`; signed URLs for `app-private` |
| Rails Active Storage | The main listener | Its own token, signed with the test signer's key |

### Go servers

Set `STORAGE_EMULATOR_HOST: gcs:4444`; the code stays as it is. With that variable Go storage sends requests without credentials, and the trusted listener runs them as `app@example.invalid`. Links that the server returns, such as a resumable upload's session URL, keep the `gcs:4444` host.

### Signing URLs for the browser

Save the test signer's key once. The command prints a service account key file:

```sh
docker compose run --rm gcs test-signer credentials > test-signer.json
jq -r .private_key test-signer.json > test-signer.pem
```

Give `test-signer.pem` to your servers the way they read other local files, for example with a read-only mount. Sign for the address the browser uses, never for `gcs:4444`, which the browser cannot reach:

```go
key, err := os.ReadFile("test-signer.pem")
if err != nil {
    return err
}
url, err := storage.SignedURL("app-private", "uploads/photo.jpg", &storage.SignedURLOptions{
    Scheme:         storage.SigningSchemeV4,
    GoogleAccessID: "test-signer@gcs-mimic.invalid",
    PrivateKey:     key,
    Method:         "PUT",
    ContentType:    "image/jpeg",
    Expires:        time.Now().Add(15 * time.Minute),
    Hostname:       os.Getenv("GCS_BROWSER_HOST"), // localhost:4443
    Insecure:       true,
})
```

The browser must then send the signed `Content-Type`. Always set `Hostname`: without it, Go storage v1.70.0 signs for the `STORAGE_EMULATOR_HOST` address, `gcs:4444`. It signs `localhost` without the port, and `GCS_MIMIC_SIGNED_HOST_PORT: lenient` accepts that; the first such request logs one `signature_host_port_omitted` warning. `gcs-mimic url sign`, as in the [signed URL guide](../../docs/en/signed-urls.md#built-in-test-signer), signs the port too.

### Rails Active Storage

Rails always sends its own token, signed with the key in its credentials, so it authenticates the same way on either listener. Point it at the main listener: its `endpoint` is also the address of the URLs it hands the browser. Save the key as above into the app, then add an entry with the same non-Google `universe_domain` that the [Rails guide](../../docs/en/rails-active-storage.md) uses:

```sh
docker compose run --rm gcs test-signer credentials | jq -r .private_key > config/gcs-mimic-test-signer.pem
```

```yaml
gcs_emulator:
  service: GCS
  project: local-project
  bucket: app-private
  endpoint: <%= ENV.fetch("GCS_EMULATOR_ENDPOINT", "http://localhost:4443/") %>
  universe_domain: gcs-emulator.invalid
  credentials:
    type: service_account
    project_id: local-project
    client_email: test-signer@gcs-mimic.invalid
    universe_domain: gcs-emulator.invalid
    private_key: <%= File.read(Rails.root.join("config/gcs-mimic-test-signer.pem")).dump if Rails.env.local? %>
```

When Rails runs in a container, follow [the guide's container section](../../docs/en/rails-active-storage.md#4-when-rails-runs-in-a-container) so that Rails and the browser use one address. The stock service signs V2 URLs, which do not sign the host. For V4 URLs signed for the final browser host and port, select the `EmulatorGcs` adapter the guide describes.

### Browser

Pages use `http://localhost:${GCS_MIMIC_PORT:-4443}`: `/app-public/OBJECT` reads a public object, and a private object answers 401 unless the page has a signed URL. The trusted port is not published, and it refuses browser requests anyway.

## Change the buckets

Edit `gcs_buckets`, or set another `FRONTEND_PORT`, then recreate the container:

```sh
docker compose up -d --force-recreate gcs
```

With Docker Compose 5.6.1, a plain `docker compose up -d` keeps the running container when only an inline config changed, and `docker compose restart` keeps the old content. On the next start, reconcile mode writes the fields each entry sets and the ready event counts the changed buckets in `buckets_file.reconciled`. Removing a field never resets it: write the value you want instead, such as `"iamPolicy": {"bindings": []}`. Removing an entry never deletes its bucket.

## Security

- Never publish port 4444 or add it to `ports:`. The browser and Host checks keep web pages out, but any other program that reaches the port acts as `app@example.invalid` without credentials.
- The test signer's private key is public: it ships in every gcs-mimic image and in the repository. Anyone can sign as `test-signer@gcs-mimic.invalid`, so anything that reaches the main listener can use its role. Use this recipe only for local development and tests, never on a shared or exposed server, and never as a Google credential.
- `app@example.invalid` and the test signer are local identities. Nothing here contacts Google.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| 401 for a request from one of your servers | It reached the main listener (`gcs:4443`, `localhost:4443`) without credentials, where it is anonymous | Set `STORAGE_EMULATOR_HOST: gcs:4444`. |
| 401 in the browser for an `app-private` object | The page used an unsigned URL for a private bucket; that is the production behavior | Hand the page a signed URL, or store the object in `app-public`. |
| 403 `SignatureDoesNotMatch` | The URL was signed for another host than the browser sends (`gcs:4444`, `127.0.0.1` instead of `localhost`), was changed after signing, or the request sends a signed header, such as `Content-Type`, with another value. Lenient forgives only a missing port | Sign for `GCS_BROWSER_HOST` and send the signed headers unchanged. |
| 403 "The trusted listener serves the application's own servers and refuses browser-initiated requests" from port 4444 | The request carried `Origin` or `Sec-Fetch-Site`: a browser, or a server that forwards a browser's headers | Send browser requests to the published port, and drop those headers from server requests. |
| 421 `misdirected` from port 4444 | Its Host is a dotted name, such as `host.docker.internal:4444` or a network alias like `storage.compose.internal`, which a DNS rebinding page could also use | Use `gcs:4444`, or list the name in `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`. |
| The browser reports a CORS error | The page's origin is not `http://localhost:${FRONTEND_PORT:-3000}`, the request sets a header the bucket's `responseHeader` does not list, or an edited `gcs_buckets` was not applied | Set `FRONTEND_PORT`, list the header, and run `docker compose up -d --force-recreate gcs`. |
| Startup reports `Unknown setting` for `GCS_MIMIC_TEST_SIGNER` and the trusted listener settings, or `test-signer credentials` says `Options require --name VALUE pairs` | The image is 0.13.0 or older | Use a newer release. |
| `cannot create config ... in read-only service gcs` | The service has `read_only: true` | Remove it; inline configs need a writable root. |

## What is verified

`scripts/smoke-containers.sh` starts this file with the image built from the checkout and checks the ready event, the two buckets and their policies, credential-free reads and writes through `gcs:4444` from another container, anonymous 401 and `allUsers` reads with CORS on the published port, a URL signed for `localhost` without the port (as Go storage signs it) and one with the port, a tampered query, a signed browser upload with its preflight, the trusted listener's 403 and 421 refusals, `test-signer credentials`, and a reconciled `FRONTEND_PORT` change. It sends requests with curl: it runs no Go client, no Rails application and no browser. The trusted listener's design and tests are in its [evidence](../../docs/en/evidence/trusted-listener.md), and [container usage](../../docs/en/container-usage.md#keep-private-buckets-private-with-a-trusted-listener) explains the trusted listener in detail.
