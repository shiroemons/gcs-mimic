# Full application example

[compose.yaml](compose.yaml) shows how a real application's services connect to the [strict local recipe](../strict-local/README.md): a Go API, a Rails app and a browser frontend. `build: ./api`, `build: ./web` and `build: ./frontend` stand for your own services.

The file shows wiring: which address each service uses, with which credentials, and why. It does not list every setting. The dictionary of every setting is the settings table in [native binary and configuration](../../docs/en/native-binary.md#native-binary-and-configuration), with each default, and the [configuration reference](../../docs/en/fake-gcs-server-configuration.md) maps fake-gcs-server's options to those settings. Every line in compose.yaml either connects two services or departs from a default, so a line you do not find there is a default you can look up.

This example needs a gcs-mimic release newer than 0.13.0, like the recipe. Until one is out, build an image from a checkout with `docker build -t gcs-mimic:local .` and set `GCS_MIMIC_IMAGE=gcs-mimic:local`.

## Start it

Save the built-in test signer's key once, beside compose.yaml. The API signs browser URLs with it, and Rails signs its own token with it:

```sh
docker compose run --rm gcs test-signer credentials > test-signer.json
jq -r .private_key test-signer.json > test-signer.pem
docker compose up -d --wait
```

The key is the same in every gcs-mimic image, so the file holds nothing secret; Compose mounts it into `api` and `web` as the secret `/run/secrets/gcs-test-signer`. Nothing else needs preparing: no certificates, no generated keys and no service account.

Four variables choose the ports on your machine:

| Variable | Default | Used by |
| --- | --- | --- |
| `GCS_MIMIC_PORT` | 4443 | The emulator, on both sides of its port mapping |
| `FRONTEND_PORT` | 3000 | The frontend's origin, also written into the buckets' CORS rules |
| `WEB_PORT` | 3001 | The Rails pages' origin, also written into the private bucket's CORS rule |
| `API_PORT` | 8080 | The Go API that the frontend calls |

A second worktree sets other values and its own `COMPOSE_PROJECT_NAME`, for example in a `.env` file beside compose.yaml.

## The services

### gcs

The recipe's service with the [per-worktree port variant](../strict-local/README.md#rails-in-a-container) for Rails:

- `ports` maps `GCS_MIMIC_PORT` to the same port in the container, and `GCS_MIMIC_LISTEN` makes the main listener use it. Rails can then give the browser the URL it uses itself.
- The network alias `gcs.localhost` lets containers reach the emulator under a name that browsers send to your machine.
- The private bucket's CORS rule admits the Rails pages' origin as well as the frontend's, because Rails direct uploads come from Rails pages.

Port 4444, the trusted listener, stays unpublished.

### api: Go

| Line | Why |
| --- | --- |
| `STORAGE_EMULATOR_HOST: gcs:4444` | Go storage sends requests without credentials to the trusted listener, which runs them as the built-in `trusted-app@gcs-mimic.invalid`, with `roles/storage.admin` on `local-project`. Objects it writes have that identity as owner. |
| `GCS_BROWSER_HOST`, `GCS_SIGNER_EMAIL`, `GCS_SIGNER_KEY_FILE` | The API's own settings for the URLs it signs for the browser, read by its code, not by gcs-mimic or Go storage. |
| `FRONTEND_ORIGIN` | The API's own CORS setting for the frontend's calls to it. |
| `depends_on` with `service_healthy` | The API starts once the emulator answers its healthcheck. |

The signing code:

```go
key, err := os.ReadFile(os.Getenv("GCS_SIGNER_KEY_FILE"))
if err != nil {
    return err
}
url, err := storage.SignedURL("app-private", name, &storage.SignedURLOptions{
    Scheme:         storage.SigningSchemeV4,
    GoogleAccessID: os.Getenv("GCS_SIGNER_EMAIL"),
    PrivateKey:     key,
    Method:         "PUT", // "GET" to show an object
    ContentType:    "image/jpeg", // PUT only; the browser must send it
    Expires:        time.Now().Add(15 * time.Minute),
    Hostname:       os.Getenv("GCS_BROWSER_HOST"),
    Insecure:       true,
})
```

`Hostname` is the address the browser uses, never `gcs:4444`. Go storage signs it without the port, which the emulator accepts because the enabled test signer makes `signed_host_port` `lenient` by default.

### web: Rails

| Line | Why |
| --- | --- |
| `GCS_EMULATOR_ENDPOINT: http://gcs.localhost:${GCS_MIMIC_PORT:-4443}/` | Rails' requests and the URLs it hands the browser share this endpoint on the main listener. Rails sends its own token there, signed with the test signer's key, so it needs no trusted listener. |
| `GCS_EMULATOR_KEY_FILE` | Where `config/storage.yml` reads the key. |
| `depends_on` with `service_healthy` | Rails starts once the emulator answers its healthcheck. |

The `config/storage.yml` entry, as in the [Rails guide](../../docs/en/rails-active-storage.md), with the key read from the secret:

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
    private_key: <%= File.read(ENV.fetch("GCS_EMULATOR_KEY_FILE")).dump if Rails.env.local? %>
```

The test signer holds `roles/storage.objectUser` on `local-project`, enough for uploads, downloads, `purge` and direct uploads.

### frontend

`PUBLIC_BUCKET_URL` is where the frontend builds public image URLs, and `API_ORIGIN` is where it asks for signed URLs. Both are its own settings. It holds no storage credentials and does not wait for the emulator.

## Browser flows

The browser reaches the emulator only on `http://localhost:${GCS_MIMIC_PORT:-4443}` (or `gcs.localhost` on the same port), where a request without credentials is anonymous.

### Reading a public image

1. The API uploads `images/logo.png` to `app-public` through `gcs:4444`.
2. The frontend renders `<img src="http://localhost:4443/app-public/images/logo.png">`, built from `PUBLIC_BUCKET_URL`.
3. The bucket's policy grants `allUsers` `roles/storage.objectViewer`, so the anonymous request gets the image. An `<img>` needs no CORS; a `fetch()` from `http://localhost:3000` gets `Access-Control-Allow-Origin` from the bucket's `GET` rule.

### Uploading to the private bucket through a signed URL

1. The frontend asks the API for an upload URL and says which `Content-Type` it will send.
2. The API signs a `PUT` for `GCS_BROWSER_HOST` with that type, as above.
3. The browser sends a preflight `OPTIONS`; the private bucket's CORS rule allows `PUT` and `Content-Type` from `http://localhost:3000`.
4. The browser sends the `PUT` with the signed `Content-Type`. The signature identifies the test signer, whose role authorizes the write.
5. The API reads the object back through `gcs:4444`.

A Rails direct upload works the same way from a Rails page at `http://localhost:3001`: Active Storage's JavaScript asks Rails for the URL, and the preflight lists the headers Rails signs, which the CORS rule allows.

### Showing a private object through a signed URL

The API signs a `GET` for the object and the frontend uses the URL in an `<img>` or a link. Without the signature the same address answers 401, as in production. In Rails, `url_for(user.avatar)` redirects the browser to a URL signed on `gcs.localhost:4443`; the browser sends `*.localhost` names to your machine, where the port is published.

## Change the buckets or ports

Edit `GCS_MIMIC_BUCKETS`, or set another port, then run `docker compose up -d`. A changed environment makes Compose recreate the container, and the inline buckets are reconciled into the existing volume: each start writes the fields an entry sets. See [Change the buckets](../strict-local/README.md#change-the-buckets) for what removing a field or an entry does.

## What is verified

`scripts/smoke-containers.sh` runs this file with [compose.smoke.yaml](compose.smoke.yaml), a test-only override that your application never needs. It replaces `api`, `web` and `frontend` with idle containers from the pinned curl image and runs each service's requests inside it, with the environment, network and secret this file gives that service. Each run picks a free port other than 4443 for `GCS_MIMIC_PORT`. The test checks:

- the key from `test-signer credentials`, mounted into `api` and `web`;
- the published port on both sides, the read-only root, and a ready event with the built-in trusted principal, the enabled test signer, `lenient` host ports and two created buckets;
- as the API: credential-free writes and reads of `app-private` and `app-public` through `gcs:4444`, and `trusted-app@gcs-mimic.invalid` as the owner of what it wrote;
- as Rails: through `gcs.localhost` and the published port, as resolved by the network alias, 401 for `app-private` and 200 for `app-public`;
- as the browser, from the host: the public image with CORS, 401 for an unsigned private object, a URL signed without the port (as Go storage signs it), one signed with the port, one signed on `gcs.localhost`, a signed `PUT` with its preflight, and the preflight of a Rails direct upload;
- after a `FRONTEND_PORT` change, a plain `docker compose up -d` recreating `gcs`, reconciling both buckets in the existing volume, allowing the new origin and no longer the old one, with the objects kept.

It runs no Go client, no Rails application and no browser.
