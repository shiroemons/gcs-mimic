"""Emulator-neutral official Python SDK probes; stdout is JSONL evidence."""

import base64
from datetime import datetime, timedelta, timezone
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import secrets
import signal
import sys
from urllib.parse import urlsplit

from google.api_core import exceptions
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
from google.oauth2 import service_account
import requests

ORIGIN = os.environ["SCORECARD_ENDPOINT"].rstrip("/")
PARTS = urlsplit(ORIGIN)
assert PARTS.scheme in ("http", "https") and PARTS.hostname in ("127.0.0.1", "localhost", "::1")
assert not PARTS.path and not PARTS.query and not PARTS.fragment and not PARTS.username
PROJECT = os.environ.get("SCORECARD_PROJECT", "local-project")
KEY = Path(os.environ.get("SCORECARD_SIGNING_KEY", Path(__file__).resolve().parents[2] / "fixtures/signing/test-only-private.pem"))
SIGNER = service_account.Credentials.from_service_account_info({
    "type": "service_account", "client_email": "local-admin@example.invalid",
    "private_key": KEY.read_text(), "token_uri": "https://oauth2.googleapis.com/token",
})


class LocalHTTP(requests.Session):
    def __init__(self):
        super().__init__()
        self.trust_env = False
        self.verify = os.environ.get("SCORECARD_CA_CERT", True)

    def send(self, request, **kwargs):
        parts = urlsplit(request.url)
        if (parts.scheme, parts.netloc) != (PARTS.scheme, PARTS.netloc):
            raise ValueError("request escaped emulator")
        kwargs["timeout"] = 20
        kwargs["allow_redirects"] = False
        return super().send(request, **kwargs)


HTTP = LocalHTTP()
os.environ["STORAGE_EMULATOR_HOST"] = ORIGIN
CLIENT = storage.Client(project=PROJECT, credentials=AnonymousCredentials(), _http=HTTP,
                        client_options={"api_endpoint": ORIGIN})
if sys.argv[1:] == ["--transport-check"]:
    try:
        HTTP.get("https://storage.googleapis.com/")
        raise AssertionError("external origin accepted")
    except ValueError as error:
        assert str(error) == "request escaped emulator"
    assert HTTP.get(ORIGIN + "/").status_code == 200
    print("PASS: Python transport CA/SAN and exact-origin guard")
    sys.exit()
CASES = []


def probe(id):
    def register(run):
        CASES.append((id, run))
        return run
    return register


def check(condition, message="unexpected result"):
    if not condition:
        raise AssertionError(message)


def expect_error(kind, call):
    try:
        call()
    except kind:
        return
    raise AssertionError(f"expected {kind.__name__}, got success")


def unique():
    return f"sc-py-{secrets.token_hex(6)}"


def bucket(**kwargs):
    return CLIENT.create_bucket(unique(), retry=None, **kwargs)


def upload(b, name, data, **kwargs):
    blob = b.blob(name)
    blob.upload_from_string(data, retry=None, **kwargs)
    return blob


def read(blob, **kwargs):
    return blob.download_as_bytes(retry=None, **kwargs)


def names(blobs):
    return sorted(blob.name for blob in blobs)


def seed(b, objects):
    for name in objects:
        upload(b, name, name)


@probe("bucket.create_get_delete")
def bucket_crud():
    b = bucket()
    check(CLIENT.get_bucket(b.name).name == b.name)
    b.delete()
    check(not b.exists())


@probe("bucket.list_prefix_paging")
def bucket_paging():
    prefix = unique()
    for i in range(3):
        CLIENT.create_bucket(f"{prefix}-{i}")
    found = []
    for page in CLIENT.list_buckets(prefix=prefix, page_size=2).pages:
        values = list(page)
        check(len(values) <= 2, "bucket page exceeds 2")
        found.extend(names(values))
    check(sorted(found) == [f"{prefix}-{i}" for i in range(3)], str(found))


@probe("bucket.update_labels")
def labels():
    b = CLIENT.bucket(unique())
    b.labels = {"a": "1", "b": "2"}
    b.create()
    b.labels = {"b": "2", "c": "3"}
    b.patch()
    b.reload()
    check(b.labels == {"b": "2", "c": "3"}, str(b.labels))


@probe("bucket.metageneration_precondition")
def bucket_condition():
    b = bucket()
    b.storage_class = "NEARLINE"
    expect_error(exceptions.PreconditionFailed, lambda: b.patch(if_metageneration_match=999))


@probe("bucket.versioning_history")
def versioning():
    b = CLIENT.bucket(unique())
    b.versioning_enabled = True
    b.create()
    first = upload(b, "v.txt", "one")
    upload(b, "v.txt", "two")
    check(len(list(b.list_blobs(versions=True))) == 2, "version count")
    check(read(b.blob("v.txt", generation=first.generation)) == b"one", "old bytes")


@probe("bucket.lifecycle")
def lifecycle():
    b = bucket()
    b.add_lifecycle_delete_rule(age=30)
    b.patch()
    b.reload()
    rules = list(b.lifecycle_rules)
    check(len(rules) == 1 and rules[0]["condition"]["age"] == 30, str(rules))


@probe("bucket.cors")
def cors():
    b = bucket()
    b.cors = [{"origin": ["https://app.example"], "method": ["GET"], "maxAgeSeconds": 3600}]
    b.patch()
    b.reload()
    check(len(b.cors) == 1 and b.cors[0]["origin"] == ["https://app.example"], str(b.cors))


@probe("bucket.retention_policy")
def retention_policy():
    b = bucket()
    b.retention_period = 3600
    b.patch()
    blob = upload(b, "r", "r")
    expect_error(exceptions.Forbidden, blob.delete)


@probe("bucket.retention_lock")
def retention_lock():
    b = bucket()
    b.retention_period = 3600
    b.patch()
    b.reload()
    b.lock_retention_policy()
    b.reload()
    check(b.retention_policy_locked, "policy not locked")


@probe("bucket.uniform_bucket_level_access")
def ubla():
    b = bucket()
    b.iam_configuration.uniform_bucket_level_access_enabled = True
    b.patch()
    b.reload()
    check(b.iam_configuration.uniform_bucket_level_access_enabled, "UBLA not enabled")


@probe("bucket.public_access_prevention")
def pap():
    b = bucket()
    b.iam_configuration.public_access_prevention = "enforced"
    b.patch()
    b.reload()
    check(b.iam_configuration.public_access_prevention == "enforced", "PAP not enforced")


@probe("bucket.iam_policy")
def iam():
    b = bucket()
    policy = b.get_iam_policy(requested_policy_version=3)
    policy.bindings.append({"role": "roles/storage.objectViewer", "members": {"user:someone@example.invalid"}})
    b.set_iam_policy(policy)
    policy = b.get_iam_policy(requested_policy_version=3)
    check(any(binding["role"] == "roles/storage.objectViewer" and "user:someone@example.invalid" in binding["members"]
              for binding in policy.bindings), "missing binding")


@probe("bucket.iam_test_permissions")
def permissions():
    check(bucket().test_iam_permissions(["storage.buckets.get", "storage.objects.list"]), "no permissions")


@probe("bucket.soft_delete_policy")
def soft_delete_policy():
    b = bucket()
    b.soft_delete_policy.retention_duration_seconds = 864000
    b.patch()
    b.reload()
    check(b.soft_delete_policy.retention_duration_seconds == 864000, "soft delete policy differs")


@probe("bucket.autoclass")
def autoclass():
    b = CLIENT.bucket(unique())
    b.autoclass_enabled = True
    b.create()
    b.reload()
    check(b.autoclass_enabled, "autoclass not enabled")


def acl_roundtrip(acl, entity):
    acl.entity(entity).grant_read()
    acl.save()
    acl.reload()
    check(any(entry["entity"] == entity and entry["role"] == "READER" for entry in acl), "ACL missing")


@probe("bucket.acl")
def bucket_acl():
    acl_roundtrip(bucket().acl, "allAuthenticatedUsers")


@probe("bucket.default_object_acl")
def default_acl():
    acl_roundtrip(bucket().default_object_acl, "allAuthenticatedUsers")


@probe("bucket.notifications")
def notifications():
    b = bucket()
    notification = b.notification(topic_name="scorecard", payload_format="JSON_API_V1")
    notification.create()
    check(notification.notification_id in [n.notification_id for n in b.list_notifications()], "notification missing")
    notification.delete()


@probe("project.hmac_keys")
def hmac():
    key, secret = CLIENT.create_hmac_key("sa@example.invalid")
    check(secret and key.access_id, "empty HMAC key")
    key.state = "INACTIVE"
    key.update()
    key.delete()


@probe("project.service_account")
def service_account_email():
    check("@" in CLIENT.get_service_account_email(), "service account missing")


@probe("object.write_read_small")
def small():
    check(read(upload(bucket(), "dir/日本語 +%?.txt", "hello")) == b"hello", "bytes differ")


@probe("object.resumable_large")
def resumable():
    b = bucket()
    data = b"a" * (3 * 1024 * 1024 + 123)
    blob = b.blob("big.bin", chunk_size=256 * 1024)
    with blob.open("wb", retry=None) as output:
        output.write(data)
    blob.reload()
    check(blob.size == len(data), "size differs")
    check(read(blob) == data, "bytes differ")


@probe("object.metadata_on_write")
def metadata_on_write():
    blob = bucket().blob("meta.txt")
    blob.cache_control = "no-cache"
    blob.content_disposition = "attachment"
    blob.content_language = "ja"
    blob.metadata = {"k": "v"}
    blob.upload_from_string("x", content_type="text/plain", retry=None)
    blob.reload()
    check((blob.content_type, blob.cache_control, blob.content_disposition, blob.content_language, blob.metadata)
          == ("text/plain", "no-cache", "attachment", "ja", {"k": "v"}), "metadata differs")


@probe("object.checksum_rejected")
def bad_crc():
    b = bucket()
    blob = b.blob("crc.txt")
    # A supplied checksum is sent by the public resumable upload API above 8 MiB.
    try:
        blob.upload_from_file(io.BytesIO(b"a" * (8 * 1024 * 1024 + 1)),
                              crc32c_checksum_value="AAAAAA==", retry=None)
    except exceptions.BadRequest as error:
        check("checksum" in str(error).lower() or "crc32c" in str(error).lower(), str(error))
    else:
        raise AssertionError("wrong CRC32C accepted")
    check(not blob.exists(), "rejected upload left an object")


@probe("object.md5_accepted")
def md5():
    data = b"md5 payload"
    blob = upload(bucket(), "md5.txt", data, checksum="md5")
    blob.reload()
    check(blob.md5_hash == base64.b64encode(hashlib.md5(data).digest()).decode(), "MD5 differs")


@probe("object.range_reads")
def ranges():
    blob = upload(bucket(), "range.txt", "0123456789")
    check(read(blob, start=2, end=5) == b"2345", "closed range")
    check(read(blob, start=7) == b"789", "open range")
    check(read(blob, start=-3) == b"789", "suffix range")


@probe("object.update_metadata")
def update_metadata():
    blob = bucket().blob("u.txt")
    blob.metadata = {"a": "1", "b": "2"}
    blob.upload_from_string("x", retry=None)
    blob.content_type = "text/csv"
    blob.metadata = {"a": None, "c": "3"}
    blob.patch()
    blob.reload()
    check(blob.content_type == "text/csv" and blob.metadata == {"b": "2", "c": "3"}, str(blob.metadata))


@probe("object.delete_not_found")
def delete():
    blob = upload(bucket(), "d.txt", "x")
    blob.delete()
    check(not blob.exists(), "deleted object visible")
    expect_error(exceptions.NotFound, blob.delete)


@probe("object.does_not_exist_precondition")
def create_condition():
    b = bucket()
    blob = upload(b, "once.txt", "first", if_generation_match=0)
    expect_error(exceptions.PreconditionFailed, lambda: upload(b, "once.txt", "second", if_generation_match=0))
    check(read(blob) == b"first", "precondition changed bytes")


@probe("object.generation_match_precondition")
def generation_condition():
    blob = upload(bucket(), "g.txt", "x")
    expect_error(exceptions.PreconditionFailed, lambda: blob.delete(if_generation_match=blob.generation + 1))
    blob.delete(if_generation_match=blob.generation)


@probe("object.metageneration_match_precondition")
def metageneration_condition():
    blob = upload(bucket(), "m.txt", "x")
    blob.content_type = "text/plain"
    expect_error(exceptions.PreconditionFailed, lambda: blob.patch(if_metageneration_match=blob.metageneration + 5))


@probe("object.list_prefix_delimiter")
def delimiter():
    b = bucket()
    seed(b, ["a/1", "a/2", "b/1", "c"])
    iterator = b.list_blobs(delimiter="/")
    check(names(iterator) == ["c"], "names differ")
    check(sorted(iterator.prefixes) == ["a/", "b/"], "prefixes differ")
    check(names(b.list_blobs(prefix="a/")) == ["a/1", "a/2"], "prefix listing")


@probe("object.list_paging")
def object_paging():
    b = bucket()
    seed(b, ["p1", "p2", "p3", "p4", "p5"])
    found, count = [], 0
    for page in b.list_blobs(page_size=2).pages:
        values = list(page)
        check(len(values) <= 2, "object page exceeds 2")
        found.extend(names(values))
        count += 1
    check(count >= 3 and sorted(found) == ["p1", "p2", "p3", "p4", "p5"], str(found))


@probe("object.list_offsets")
def offsets():
    b = bucket()
    seed(b, ["a", "b", "c", "d"])
    check(names(b.list_blobs(start_offset="b", end_offset="d")) == ["b", "c"], "offset listing")


@probe("object.list_match_glob")
def glob():
    b = bucket()
    seed(b, ["x/a.txt", "x/b.csv", "y/c.txt"])
    check(names(b.list_blobs(match_glob="**/*.txt")) == ["x/a.txt", "y/c.txt"], "glob listing")


@probe("object.copy_with_metadata")
def copy():
    b, destination = bucket(), bucket()
    source = upload(b, "src", "copy me")
    # copy_blob cannot supply destination metadata; rewrite exposes it publicly.
    dest = destination.blob("dst")
    dest.content_type = "text/plain"
    dest.metadata = {"copied": "yes"}
    token = None
    while True:
        token, _, _ = dest.rewrite(source, token=token)
        if token is None:
            break
    dest.reload()
    check(dest.content_type == "text/plain" and dest.metadata == {"copied": "yes"}, "copy metadata")
    check(read(dest) == b"copy me", "copy bytes")


@probe("object.rewrite_storage_class")
def rewrite():
    b = bucket()
    data = b"a" * (2 * 1024 * 1024 + 7)
    source = upload(b, "src", data)
    dest = b.blob("dst")
    dest.storage_class = "NEARLINE"
    token = None
    while True:
        token, _, _ = dest.rewrite(source, token=token)
        if token is None:
            break
    dest.reload()
    check(dest.storage_class == "NEARLINE", "storage class differs")
    check(read(dest) == data, "rewrite bytes")


@probe("object.compose")
def compose():
    b = bucket()
    seed(b, ["p1", "p2", "p3"])
    dest = b.blob("all")
    dest.compose([b.blob(name) for name in ["p1", "p2", "p3"]])
    check(dest.component_count == 3, "component count")
    check(read(dest) == b"p1p2p3", "compose bytes")


@probe("object.move")
def move():
    b = bucket()
    blob = upload(b, "old", "moved")
    b.move_blob(blob, "new")
    check(not b.blob("old").exists(), "source remains")
    check(read(b.blob("new")) == b"moved", "move bytes")


@probe("object.acl")
def object_acl():
    acl_roundtrip(upload(bucket(), "acl.txt", "x").acl, "allUsers")


@probe("object.holds")
def holds():
    blob = upload(bucket(), "hold.txt", "x")
    blob.temporary_hold = True
    blob.patch()
    expect_error(exceptions.Forbidden, blob.delete)
    blob.temporary_hold = False
    blob.patch()
    blob.delete()


@probe("object.retention")
def object_retention():
    blob = upload(bucket(enable_object_retention=True), "ret.txt", "x")
    blob.retention.mode = "Unlocked"
    blob.retention.retain_until_time = datetime.now(timezone.utc) + timedelta(hours=1)
    blob.patch()
    blob.reload()
    check(blob.retention.mode == "Unlocked", "retention not stored")
    expect_error(exceptions.Forbidden, blob.delete)


@probe("object.custom_time")
def custom_time():
    blob = upload(bucket(), "ct.txt", "x")
    want = datetime(2026, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
    blob.custom_time = want
    blob.patch()
    blob.reload()
    check(blob.custom_time == want, "customTime differs")


@probe("object.storage_class_on_write")
def storage_class():
    blob = bucket().blob("cold")
    blob.storage_class = "COLDLINE"
    blob.upload_from_string("x", retry=None)
    blob.reload()
    check(blob.storage_class == "COLDLINE", "storage class differs")


@probe("object.gzip_transcoding")
def transcoding():
    plain = b"compress me " * 50
    zipped = gzip.compress(plain)
    blob = bucket().blob("gz.txt")
    blob.content_encoding = "gzip"
    blob.upload_from_string(zipped, content_type="text/plain", retry=None)
    check(read(blob) == plain, "decompressed bytes")
    check(read(blob, raw_download=True) == zipped, "compressed bytes")


@probe("object.customer_supplied_key")
def csek():
    b = bucket()
    blob = b.blob("csek.txt", encryption_key=secrets.token_bytes(32))
    blob.upload_from_string("secret", retry=None)
    expect_error(exceptions.GoogleAPICallError, lambda: read(b.blob("csek.txt")))
    check(read(blob) == b"secret", "keyed bytes")


@probe("object.soft_delete_restore")
def restore():
    b = bucket()
    b.soft_delete_policy.retention_duration_seconds = 604800
    b.patch()
    blob = upload(b, "sd.txt", "restore me")
    generation = blob.generation
    blob.delete()
    check(names(b.list_blobs(soft_deleted=True)) == ["sd.txt"], "soft-deleted listing")
    b.restore_blob("sd.txt", generation)
    check(read(b.blob("sd.txt")) == b"restore me", "restored bytes")


def signed(version, method):
    b = bucket()
    blob = b.blob("signed.txt")
    if method == "GET":
        blob.upload_from_string("signed", retry=None)
    url = blob.generate_signed_url(version=version, expiration=timedelta(minutes=5),
                                   method=method, api_access_endpoint=ORIGIN, credentials=SIGNER)
    response = HTTP.request(method, url, data=b"uploaded" if method == "PUT" else None)
    check(response.status_code == 200, f"status {response.status_code}: {response.text[:200]}")
    check(response.content == b"signed" if method == "GET" else read(blob) == b"uploaded", "signed bytes")


probe("signed.v4_get")(lambda: signed("v4", "GET"))
probe("signed.v4_put")(lambda: signed("v4", "PUT"))
probe("signed.v2_get")(lambda: signed("v2", "GET"))


@probe("signed.post_policy_v4")
def post_policy():
    b = bucket()
    policy = CLIENT.generate_signed_post_policy_v4(b.name, "post.txt", expiration=timedelta(minutes=5), credentials=SIGNER)
    response = HTTP.post(f"{ORIGIN}/{b.name}", data=policy["fields"], files={"file": ("post.txt", b"posted")})
    check(200 <= response.status_code < 300, f"status {response.status_code}: {response.text[:200]}")
    check(read(b.blob("post.txt")) == b"posted", "posted bytes")


def timed_out(signum, frame):
    raise TimeoutError("case exceeded 60 seconds")


signal.signal(signal.SIGALRM, timed_out)
for id, run in CASES:
    if os.environ.get("SCORECARD_RUN", "") not in id:
        continue
    result = {"sdk": "python", "id": id, "area": "bucket" if id.startswith("project.") else id.split(".")[0], "pass": True}
    try:
        signal.alarm(60)
        run()
    except Exception as error:
        response = getattr(error, "response", None)
        detail = response.text if response is not None else str(error)
        result.update({"pass": False, "detail": f"{type(error).__name__}: {detail}"[:300]})
    finally:
        signal.alarm(0)
    print(json.dumps(result, ensure_ascii=False), flush=True)
