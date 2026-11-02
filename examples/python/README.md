# Python example

`main.py` uses the official `google-cloud-storage` package (3.17.1) to create a
bucket, upload a text object, list the bucket, download the object and delete
both. It talks only to a local gcs-mimic and never contacts Google.

## Prerequisites

- [uv](https://docs.astral.sh/uv/) 0.13.6 or newer; it installs the pinned
  CPython 3.15.2 (see `.python-version`) if needed
- Docker, to start gcs-mimic

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
./run.sh
```

`run.sh` installs the packages pinned in `uv.lock` (`uv run --locked`, which
fails instead of updating the lock) and runs `main.py` against
`GCS_MIMIC_ENDPOINT` (default `http://localhost:4443`). It accepts only a
loopback `http://` origin. The output looks like this:

```text
Created bucket example-python-1a2b3c4d
Uploaded hello.txt
Objects: hello.txt
Downloaded hello.txt: Hello from Python!
Deleted bucket example-python-1a2b3c4d
```

Stop the emulator with `docker rm -f gcs-mimic`.

## Client setup

```python
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
client = storage.Client(project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"})
```

Anonymous credentials and an explicit project keep google-auth from looking up
Google credentials or the metadata server.
