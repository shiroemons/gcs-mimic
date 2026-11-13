# Production-like local access

Paste one block into your application's `compose.yaml` to get Cloud Storage access that behaves like production during local development and tests:

- Private buckets stay private: a browser reaches them only through signed URLs.
- Public buckets are readable only through their `allUsers` grant.
- Your application's servers keep their own Cloud Storage calls: Go storage with `STORAGE_EMULATOR_HOST` sends no credentials and still gets in. URLs they hand to browsers are checked as in production, so [some applications need a change](#what-the-trusted-listener-does-not-cover).
- URLs that Go storage signs for a local port verify.
- Rails Active Storage works with the same signer.

Nothing needs preparing:

- No certificates. The default scheme serves plain HTTP and HTTPS on the same port, so clients use plain HTTP without a CA; HTTPS uses the development certificate built into the image.
- No key generation. The built-in test signer's key ships with gcs-mimic. When your application signs URLs, save it with `docker compose run --rm gcs test-signer credentials > test-signer.json`.
- No service account for Go servers. They send no credentials at all.

Rails always needs that file, even when it signs no URLs: it signs its own token with the key in its credentials.

This recipe needs gcs-mimic 0.14.0 or later. 0.13.0 stops at startup with `Unknown setting` for `GCS_MIMIC_TRUSTED_LISTEN`, `GCS_MIMIC_TEST_SIGNER` and `GCS_MIMIC_BUCKETS`, and with `auth_mode=strict requires identity_file`. To try a checkout instead of the release, build an image with `docker build -t gcs-mimic:local .` and set `GCSM_IMAGE=gcs-mimic:local`.

The [full application example](../full-stack/README.md) shows a Go API, a Rails app and a frontend connected to this service.

## Add it to your compose.yaml

Add the `gcs` service under your `services:` and the `gcs-data` entry under `volumes:`, creating that key if your file has none. The block is [compose.yaml](compose.yaml) in this directory, unchanged; `scripts/check-docs.sh` keeps the two identical, and the container smoke test runs that file.

<!-- strict-local compose.yaml -->
```yaml
services:
  # Production-like local Cloud Storage from gcs-mimic's examples/strict-local recipe.
  # Local development and tests only: the built-in test signer's private key is public.
  gcs:
    image: ${GCSM_IMAGE:-ghcr.io/shiroemons/gcs-mimic:0.21.0}
    ports:
      # Browsers use the main listener. Never publish the trusted port 4444: anything
      # that reaches it acts as trusted-app@gcs-mimic.invalid without credentials.
      - "127.0.0.1:${GCSM_PORT:-4443}:4443"
    environment:
      # Obsolete Compose names are passed only so the server refuses them with replacement advice.
      GCS_MIMIC_PORT:
      GCS_MIMIC_IMAGE:
      GCS_MIMIC_TLS_DIR:
      GCS_MIMIC_AUTH_MODE: strict
      # Application servers on the Compose network use gcs:4444 without credentials.
      GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444
      # The built-in signer, whose key ships with gcs-mimic: needed for signed URLs and by
      # Rails or any client using `test-signer credentials`. Remove it only if nothing does.
      GCS_MIMIC_TEST_SIGNER: enabled
      GCS_MIMIC_BUCKETS: |
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
      - gcs-data:/var/lib/gcs-mimic
    # The data volume is the only path the server writes.
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    healthcheck:
      # Probes the main listener, as the image's own healthcheck does.
      test: [CMD, gcs-mimic, healthcheck]
      interval: 2s
      timeout: 2s
      start_period: 2s
      retries: 15
volumes:
  gcs-data:
```

Each application server that uses Cloud Storage needs these lines. `GCS_BROWSER_HOST` is your application's own setting, read by the signing code below, not by gcs-mimic or Go storage:

```yaml
services:
  api:
    environment:
      STORAGE_EMULATOR_HOST: gcs:4444
      GCS_BROWSER_HOST: localhost:${GCSM_PORT:-4443}
    depends_on:
      gcs:
        condition: service_healthy
```

Set `FRONTEND_PORT` when your frontend does not run on port 3000, and `GCSM_PORT` when port 4443 is taken, then run `docker compose up -d --wait`.

## What each line does

| Line | What it does |
| --- | --- |
| `GCS_MIMIC_AUTH_MODE: strict` | Every request is authorized by IAM and ACLs. A request without credentials is anonymous and gets only `allUsers` grants. |
| `GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444` | A second, plain HTTP listener for your servers on the Compose network, where a request without credentials acts as the built-in identity `trusted-app@gcs-mimic.invalid`. It binds every interface inside the container so that other containers reach it; leaving it out of `ports:` keeps it off your machine's network. |
| `GCS_MIMIC_TEST_SIGNER: enabled` | Turns on the built-in signer `test-signer@gcs-mimic.invalid`, whose private key ships with gcs-mimic, so nothing has to generate or register a key. Signed URLs need it, and so do Rails and any other client that uses the key from `test-signer credentials`: they sign their own token with it, and the server knows that key only while the signer is enabled. Remove the line only when nothing signs URLs or uses those credentials. |
| `GCS_MIMIC_BUCKETS` | Creates the buckets at startup with their policy and CORS rules. Compose fills in `${FRONTEND_PORT:-3000}` before the server reads the JSON, so write a literal `$` as `$$`. |
| `ports` | Publishes only the main listener, and only on loopback. |
| `gcs-data` volume | Keeps buckets and objects across `docker compose down` and `up`. |
| `read_only`, `cap_drop`, `security_opt` | The container writes only to its data volume, where the image also keeps SQLite's temporary files, and runs without Linux capabilities or a way to gain privileges. |
| `healthcheck` | Lets services wait with `condition: service_healthy`. |

The recipe leaves these to their defaults:

- **No identity document.** Strict mode starts without `GCS_MIMIC_IDENTITIES` or `GCS_MIMIC_IDENTITY_FILE` when the trusted listener uses its built-in identity or the test signer is enabled explicitly.
- **The trusted listener's identity.** Without `GCS_MIMIC_TRUSTED_PRINCIPAL`, requests without credentials on port 4444 act as `trusted-app@gcs-mimic.invalid`, which holds `roles/storage.admin` on the project of every bucket the bucket document creates and on the seed project (`local-project` unless `GCS_MIMIC_SEED_PROJECT` says otherwise). An entry for it in the identities replaces these roles.
- **The test signer's roles.** The enabled signer holds `roles/storage.objectUser` on the same projects, unless the identities declare it. Declaring it there also enables it, so the `GCS_MIMIC_TEST_SIGNER` line becomes optional; an explicit `GCS_MIMIC_TEST_SIGNER` always wins. Startup warns once that the signer's key is published.
- **Signatures without the port.** With the test signer enabled, `signed_host_port` is `lenient` for that signer's own signatures, and ready reports `signed_host_port_signers` `test-signer`: a V4 signature by the test signer whose host leaves out the port, as Go storage signs, verifies, and the host name, path, query, signed headers and expiry stay bound. Any other key, and every signature without the test signer, is `exact`; set `GCS_MIMIC_SIGNED_HOST_PORT=lenient` to accept such signatures from every signer.
- **Edits reach existing buckets.** The bucket document uses `buckets_mode` `reconcile`, the default, so each start writes the fields an entry sets to a bucket that already exists in the volume. See [Change the buckets](#change-the-buckets).
- **No CORS without rules.** Strict mode answers XML requests to a bucket without `cors` rules with no CORS headers (`cors_fallback` `none`), as Cloud Storage does. Permissive mode allows every origin there instead.

The buckets:

| Bucket | Access | CORS for `http://localhost:${FRONTEND_PORT:-3000}` |
| --- | --- | --- |
| `app-public` | Its policy grants `allUsers` `roles/storage.objectViewer`, so anyone reads its objects without credentials. The policy replaces the default one, as `setIamPolicy` does. | `GET` and `HEAD`, so a page can `fetch()` its objects. `<img>` and other plain embeds need no CORS. |
| `app-private` | No `iamPolicy`, so it keeps the default policy, which grants nothing to `allUsers`; the two built-in identities reach it through their project roles. | `PUT` for uploads through signed URLs. A browser lists every header it sets beyond the CORS-safelisted ones in its preflight, and the bucket must allow each: `Content-Type` for any upload with a type, the others because Rails direct uploads send them. Add `GET` if pages `fetch()` signed downloads. |

### Roles

`trusted-app@gcs-mimic.invalid` has `roles/storage.admin`, so your servers and test setup can do what a project owner's service account does: create buckets, change policies, read and write every object. If your servers only read and write objects, declare the identity with a narrower role, so that a missing permission fails locally as it would in production:

```yaml
      GCS_MIMIC_IDENTITIES: |
        {"identities": [
          {"id": "trusted-app@gcs-mimic.invalid", "tokens": [],
           "project_roles": {"local-project": ["roles/storage.objectUser"]}}
        ]}
```

`test-signer@gcs-mimic.invalid` has `roles/storage.objectUser`. A signed download needs `storage.objects.get`; a signed upload needs `storage.objects.create`, and `storage.objects.delete` as well when it replaces an object. Rails also deletes (`purge`), lists (`delete_prefixed`) and updates metadata through the same identity. `roles/storage.objectUser` is the smallest predefined role with all of these; `roles/storage.objectViewer` plus `roles/storage.objectCreator` lack delete and update. It grants no bucket or IAM administration, which matters because anyone on your machine can sign as this identity.

## Connect each client

| Client | Address | Credentials |
| --- | --- | --- |
| Your servers on the Compose network | `gcs:4444` (trusted listener) | None; they act as `trusted-app@gcs-mimic.invalid` |
| Browser | `http://localhost:${GCSM_PORT:-4443}` (main listener) | None for `app-public`; signed URLs for `app-private` |
| Rails Active Storage | The main listener | Its own token, signed with the test signer's key |

### Go servers

Set `STORAGE_EMULATOR_HOST: gcs:4444`; the calls your servers make to Cloud Storage stay as they are. With that variable Go storage sends requests without credentials, and the trusted listener runs them as `trusted-app@gcs-mimic.invalid`, which also becomes the owner of the objects they write. Links that the server returns, such as a resumable upload's session URL, keep the `gcs:4444` host.

### What the trusted listener does not cover

It covers only the requests your servers send themselves. URLs they hand to a browser or another program are checked like any other request, so these applications need a change:

| Your application | What happens | Change |
| --- | --- | --- |
| Skips signing only against the emulator, and gives the browser an unsigned URL or POST form for a private bucket. fake-gcs-server checks no signature, so this shortcut is common | 401 on the published port, because the request is anonymous. Permissive `GCS_MIMIC_SIGNED_URLS=verify` cannot show this: an unsigned request has no signature to check, and it is allowed | Sign those URLs against the emulator too, as in [Signing URLs for the browser](#signing-urls-for-the-browser) |
| Signs without a key in production, leaving Go storage to find the identity in the runtime's credentials | `storage: unable to detect default GoogleAccessID`, because a client that uses `STORAGE_EMULATOR_HOST` has no credentials | Add an emulator branch that sets `GoogleAccessID`, `PrivateKey`, `Hostname` and `Insecure`, as in [Signing URLs for the browser](#signing-urls-for-the-browser) |
| Builds URLs from the value of `STORAGE_EMULATOR_HOST`, such as the address an image proxy fetches objects from | Those URLs move to `gcs:4444` with it. Whatever fetches them acts as `trusted-app@gcs-mimic.invalid`, so a private object passed by mistake still loads and the difference from production stays hidden. URLs stored before the switch keep the old port, so code that recognizes the emulator by its host and port no longer matches them | Build them from a separate setting that names the main listener, `gcs:4443`, where the fetch is anonymous as it is in production |

### Signing URLs for the browser

Save the test signer's key once. The command prints a service account key file:

```sh
docker compose run --rm gcs test-signer credentials > test-signer.json
jq -r .private_key test-signer.json > test-signer.pem
```

Give `test-signer.pem` to your servers the way they read other local files, for example with a read-only mount or a Compose secret. The key is the same in every gcs-mimic image, so the file holds nothing secret. Sign for the address the browser uses, never for `gcs:4444`, which the browser cannot reach:

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

The browser must then send the signed `Content-Type`. Always set `Hostname`: without it, Go storage v1.70.0 signs for the `STORAGE_EMULATOR_HOST` address, `gcs:4444`. It signs `localhost` without the port, which the enabled test signer's `lenient` default accepts; the first such request logs one `signature_host_port_omitted` warning. `gcs-mimic url sign`, as in the [signed URL guide](../../docs/en/signed-urls.md#built-in-test-signer), signs the port too.

### Rails Active Storage

Rails always sends its own token, signed with the key in its credentials, so it authenticates the same way on either listener. Point it at the main listener: its `endpoint` is also the address of the URLs it hands the browser. Save the key from `test-signer credentials` into the app, then add an entry with the same non-Google `universe_domain` that the [Rails guide](../../docs/en/rails-active-storage.md) uses:

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

The stock service signs V2 URLs, which do not sign the host. For V4 URLs signed for the final browser host and port, select the `EmulatorGcs` adapter the guide describes.

### Rails in a container

Rails uses one `endpoint` both for its own requests and for the URLs it gives the browser, but the browser cannot resolve the Compose service name. Give the service the name `gcs.localhost` and the same port on both sides of `ports`:

```yaml
services:
  gcs:
    ports: ["127.0.0.1:${GCSM_PORT:-4443}:${GCSM_PORT:-4443}"]
    environment:
      GCS_MIMIC_LISTEN: 0.0.0.0:${GCSM_PORT:-4443}
    networks:
      default:
        aliases: [gcs.localhost]
  web: # your Rails service
    environment:
      GCS_EMULATOR_ENDPOINT: http://gcs.localhost:${GCSM_PORT:-4443}/
```

Replace the recipe's `ports` entry with this one and add the other lines to its service. Containers resolve `gcs.localhost` through the network alias, browsers send every `*.localhost` name to your machine, and because the main listener inside the container uses the published port, `http://gcs.localhost:${GCSM_PORT:-4443}/` reaches the emulator from both. Each worktree can then set its own `GCSM_PORT` and Compose project name. The image healthcheck follows `GCS_MIMIC_LISTEN`. The trusted listener accepts `gcs.localhost:4444` as well, because it is a `.localhost` name; `gcs:4444` stays the address for your other servers. The [full application example](../full-stack/README.md) uses this variant.

### Browser

Pages use `http://localhost:${GCSM_PORT:-4443}`: `/app-public/OBJECT` reads a public object, and a private object answers 401 unless the page has a signed URL. The trusted port is not published, and it refuses browser requests anyway.

## Change the buckets

Edit `GCS_MIMIC_BUCKETS`, or set another `FRONTEND_PORT`, then run:

```sh
docker compose up -d
```

The JSON is part of the service's environment, so a changed value makes Compose recreate the container. `docker compose restart` restarts the old container with its old value. On the next start the server writes the fields each entry sets and the ready event counts the changed buckets in `buckets.reconciled`. Removing a field never resets it: write the value you want instead, such as `"iamPolicy": {"bindings": []}`. Removing an entry never deletes its bucket.

## Security

- Never publish port 4444 or add it to `ports:`. The browser and Host checks keep web pages out, but any other program that reaches the port acts as `trusted-app@gcs-mimic.invalid` without credentials.
- The test signer's private key is public: it ships in every gcs-mimic image and in the repository. Anyone can sign as `test-signer@gcs-mimic.invalid`, so anything that reaches the main listener can use its role. Use this recipe only for local development and tests, never on a shared or exposed server, and never as a Google credential.
- `trusted-app@gcs-mimic.invalid` and the test signer are local identities. Nothing here contacts Google.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| 401 "Anonymous caller does not have ..." for a request from one of your servers | It reached the main listener (`gcs:4443`, `localhost:4443`) without credentials, where it is anonymous | Set `STORAGE_EMULATOR_HOST: gcs:4444`. |
| 401 "Anonymous caller does not have ..." in the browser for an `app-private` object | The page used an unsigned URL for a private bucket; that is the production behavior | Hand the page a signed URL, or store the object in `app-public`. |
| 403 "`trusted-app@gcs-mimic.invalid` does not have ..." or "`test-signer@gcs-mimic.invalid` does not have ..." in a bucket you created later | The bucket belongs to a project that `GCS_MIMIC_BUCKETS` does not name, where the built-in identities have no role | Create it in `local-project`, or name its project in an entry of `GCS_MIMIC_BUCKETS`. |
| 403 `SignatureDoesNotMatch` | The URL was signed for another host than the browser sends (`gcs:4444`, `127.0.0.1` instead of `localhost`), was changed after signing, or the request sends a signed header, such as `Content-Type`, with another value. Lenient forgives only a missing port | Sign for `GCS_BROWSER_HOST` and send the signed headers unchanged. |
| 403 "The trusted listener serves the application's own servers and refuses browser-initiated requests" from port 4444 | The request carried `Origin` or `Sec-Fetch-Site`: a browser, or a server that forwards a browser's headers | Send browser requests to the published port, and drop those headers from server requests. |
| 421 `misdirected` from port 4444 | Its Host is a dotted name, such as `host.docker.internal:4444` or a network alias like `storage.compose.internal`, which a DNS rebinding page could also use | Use `gcs:4444` or `gcs.localhost:4444`, or list the name in `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`. |
| The browser reports a CORS error | The page's origin is not `http://localhost:${FRONTEND_PORT:-3000}`, or the request sets a header the bucket's `responseHeader` does not list | Set `FRONTEND_PORT` or list the header, then run `docker compose up -d`. |
| Rails in a container: images do not load and direct uploads fail with `Error storing ... Status: 0` | Rails handed the browser URLs on a name only containers resolve, such as `gcs` | Use the [container variant](#rails-in-a-container). |
| Compose reports `invalid interpolation format` for `GCS_MIMIC_BUCKETS` | The JSON holds a literal `$` | Write it as `$$`. |
| Startup reports `Unknown setting` for `GCS_MIMIC_TRUSTED_LISTEN`, `GCS_MIMIC_TEST_SIGNER` or `GCS_MIMIC_BUCKETS` and `auth_mode=strict requires identity_file`, or `test-signer credentials` says `Options require --name VALUE pairs` | The image is 0.13.0 or older | Use 0.14.0 or later, as compose.yaml does unless `GCSM_IMAGE` selects another image. |

## What is verified

`scripts/smoke-containers.sh` starts this file with the image built from the checkout and checks the read-only root, the ready event (strict mode, the enabled test signer, `lenient` host ports, the built-in trusted principal, two created buckets) and the buckets' policies, credential-free reads and writes through `gcs:4444` from another container and the owner of the written object, anonymous 401 and `allUsers` reads with CORS on the published port, a URL signed for `localhost` without the port (as Go storage signs it) and one with the port, a tampered query, a signed browser upload with its preflight, the trusted listener's 403 and 421 refusals, `test-signer credentials`, and a `FRONTEND_PORT` change that a plain `docker compose up -d` applies to the existing volume. It runs the [full application example](../full-stack/README.md) as well, including the `gcs.localhost` variant above. It sends requests with curl: it runs no Go client, no Rails application and no browser, and it does not run the narrower `GCS_MIMIC_IDENTITIES` entry under [Roles](#roles). `scripts/strict-recipe-browser.sh` runs Chromium against the published main listener: anonymous refusals, signed downloads and uploads, tampered and expired signatures, and the CORS rules above. The trusted listener's design and tests are in its [evidence](../../docs/en/evidence/trusted-listener.md), and [container usage](../../docs/en/container-usage.md#keep-private-buckets-private-with-a-trusted-listener) explains the trusted listener in detail.
