"""Creates a bucket, uploads, lists, downloads and deletes an object in a local gcs-mimic."""

import os
import secrets

from google.auth.credentials import AnonymousCredentials
from google.cloud import storage

endpoint = os.environ.get("GCS_MIMIC_ENDPOINT", "http://localhost:4443")

# Anonymous credentials and an explicit project keep google-auth from looking up
# Google credentials or the metadata server.
client = storage.Client(
    project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": endpoint},
)

bucket = client.create_bucket(f"example-python-{secrets.token_hex(4)}")
print(f"Created bucket {bucket.name}")
try:
    blob = bucket.blob("hello.txt")
    blob.upload_from_string("Hello from Python!", content_type="text/plain")
    print("Uploaded hello.txt")

    names = [b.name for b in client.list_blobs(bucket)]
    print(f"Objects: {', '.join(names)}")

    print(f"Downloaded hello.txt: {blob.download_as_text()}")
finally:
    # force=True deletes the remaining objects before the bucket.
    bucket.delete(force=True)
    print(f"Deleted bucket {bucket.name}")
