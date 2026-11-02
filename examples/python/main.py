import os
import secrets

from google.auth.credentials import AnonymousCredentials
from google.cloud import storage

endpoint = os.environ.get("GCS_MIMIC_ENDPOINT", "http://localhost:4443")

# Anonymous credentials for the emulator endpoint; buckets need a project.
client = storage.Client(
    project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": endpoint},
)

bucket = client.create_bucket(f"example-python-{secrets.token_hex(4)}")
print(f"created bucket {bucket.name}")

blob = bucket.blob("hello.txt")
blob.upload_from_string("Hello from gcs-mimic!")
print(f"uploaded {blob.name}")

for b in client.list_blobs(bucket):
    print(f"listed {b.name} ({b.size} bytes)")

print(f"downloaded: {blob.download_as_text()}")

blob.delete()
print(f"deleted {blob.name}")
bucket.delete()
print(f"deleted bucket {bucket.name}")
