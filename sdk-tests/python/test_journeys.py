"""Official Python SDK journeys; run once against a permissive and once against a strict server."""

from datetime import datetime, timedelta, timezone
import hashlib
import io
import os
import shutil
import tempfile
import time
import unittest

from google.api_core import exceptions
from google.cloud import storage
import requests

from harness import CHUNK_SIZE, PROJECT, GuardedSession, delete_bucket, environment, unique


class Journey(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.env = environment()
        cls.client = cls.env.client

    def new_bucket(self, prefix, configure=None, **kwargs):
        """Creates a bucket through the high-level API and registers its cleanup."""
        bucket = self.client.bucket(unique(prefix))
        if configure:
            configure(bucket)
        bucket = self.client.create_bucket(bucket, retry=None, **kwargs)
        self.addCleanup(delete_bucket, bucket)
        return bucket

    def upload(self, bucket, name, data, **kwargs):
        blob = bucket.blob(name)
        blob.upload_from_string(data, retry=None, **kwargs)
        return blob


class TransportJourneys(Journey):
    def test_transport_trust_and_origin_guard(self):
        origin = self.env.origin
        self.assertEqual(self.env.session.get(f"{origin.url}/_internal/ready").status_code, 200)
        with self.assertRaisesRegex(ValueError, "outside the emulator origin"):
            self.env.session.get("https://storage.googleapis.com/storage/v1/b")
        if origin.scheme == "https":
            # Without the emulator CA, the public trust store must reject the local certificate.
            untrusted = GuardedSession(origin)
            with self.assertRaises(requests.exceptions.SSLError):
                untrusted.get(f"{origin.url}/_internal/ready")


class BucketJourneys(Journey):
    def test_default_create_get_list_patch_put_delete(self):
        bucket = self.client.create_bucket(unique("sdk-python-basic"), retry=None)
        try:
            self.assertEqual(self.client.get_bucket(bucket.name).name, bucket.name)
            self.assertIn(bucket.name, [item.name for item in self.client.list_buckets()])
            bucket.labels = {"transport": self.env.origin.scheme}
            bucket.patch()
            bucket.reload()
            self.assertEqual(bucket.labels, {"transport": self.env.origin.scheme})
            # A resource holding only these fields makes Bucket.update a replacement PUT.
            replacement = storage.Bucket(self.client, bucket.name)
            replacement.labels = {"replacement": "yes"}
            replacement.storage_class = "NEARLINE"
            replacement.update(if_metageneration_match=bucket.metageneration)
            bucket.reload()
            self.assertEqual(bucket.labels, {"replacement": "yes"})
            self.assertEqual(bucket.storage_class, "NEARLINE")
        finally:
            bucket.delete()
        self.assertFalse(bucket.exists())

    def test_put_preserves_unrelated_settings(self):
        def configure(item):
            item.versioning_enabled = True
            item.labels = {"keep": "yes"}
        bucket = self.new_bucket("sdk-python-put", configure)
        bucket.iam_configuration.public_access_prevention = "enforced"
        bucket.update()
        bucket.reload()
        self.assertTrue(bucket.versioning_enabled)
        self.assertEqual(bucket.labels, {"keep": "yes"})
        self.assertEqual(bucket.iam_configuration.public_access_prevention, "enforced")

    def test_regional_location(self):
        bucket = self.new_bucket("sdk-python-region", location="ASIA-NORTHEAST1")
        self.assertEqual((bucket.location, bucket.location_type), ("ASIA-NORTHEAST1", "region"))
        bucket.reload()
        self.assertEqual((bucket.location, bucket.location_type), ("ASIA-NORTHEAST1", "region"))

    def test_unsupported_call_fails_fast(self):
        bucket = self.new_bucket("sdk-python-unsupported")
        started = time.monotonic()
        # The SDK's default retry policy stays in place: a 400 must not be retried.
        with self.assertRaises(exceptions.BadRequest) as raised:
            list(bucket.list_blobs(delimiter="/", include_folders_as_prefixes=True))
        self.assertLess(time.monotonic() - started, 3)
        self.assertEqual(raised.exception.code, 400)
        self.assertEqual(raised.exception.errors[0]["reason"], "badRequest")
        self.assertIn("gcs-mimic", raised.exception.message)

    def test_notifications(self):
        bucket = self.new_bucket("sdk-python-notify")
        notification = bucket.notification(
            topic_name="uploads", topic_project=PROJECT, custom_attributes={"team": "storage"},
            event_types=["OBJECT_FINALIZE", "OBJECT_DELETE"], blob_name_prefix="logs/",
            payload_format="NONE")
        notification.create()
        fetched = bucket.get_notification(notification.notification_id)
        # get_notification keeps the raw resource but does not parse topic into topic_name;
        # list_notifications builds each item with from_api_repr, which does.
        self.assertEqual(fetched._properties["topic"],
                         "//pubsub.googleapis.com/projects/local-project/topics/uploads")
        self.assertEqual(fetched.notification_id, notification.notification_id)
        (listed,) = list(bucket.list_notifications())
        self.assertEqual((listed.topic_project, listed.topic_name), (PROJECT, "uploads"))
        self.assertEqual(listed.event_types, ["OBJECT_FINALIZE", "OBJECT_DELETE"])
        self.assertEqual(listed.blob_name_prefix, "logs/")
        self.assertEqual(listed.custom_attributes, {"team": "storage"})
        self.assertEqual(listed.payload_format, "NONE")
        self.assertEqual([item.notification_id for item in bucket.list_notifications()],
                         [notification.notification_id])
        notification.delete()
        self.assertEqual(list(bucket.list_notifications()), [])

    def test_json_acl(self):
        bucket = self.new_bucket("sdk-python-acl")
        # ACL entries are stored and returned; they do not yet grant or deny access.
        bucket.acl.all_authenticated().grant_read()
        bucket.acl.save()
        bucket.acl.reload()
        self.assertIn("READER", bucket.acl.all_authenticated().get_roles())
        bucket.default_object_acl.all_authenticated().grant_read()
        bucket.default_object_acl.save()
        bucket.default_object_acl.reload()
        self.assertIn("READER", bucket.default_object_acl.all_authenticated().get_roles())
        blob = self.upload(bucket, "acl.txt", "x")
        blob.acl.reload()
        self.assertIn("READER", blob.acl.all_authenticated().get_roles(), "default ACL not inherited")
        blob.acl.all().grant_read()
        blob.acl.save()
        blob.acl.reload()
        self.assertIn("READER", blob.acl.all().get_roles())
        blob.acl.all().revoke_read()
        blob.acl.save()
        blob.acl.reload()
        self.assertNotIn("READER", blob.acl.all().get_roles())
        blob.delete()
        bucket.default_object_acl.all_authenticated().revoke_read()
        bucket.default_object_acl.save()
        bucket.acl.all_authenticated().revoke_read()
        bucket.acl.save()

    def test_autoclass(self):
        def configure(item):
            item.autoclass_enabled = True
        bucket = self.new_bucket("sdk-python-autoclass", configure)
        bucket.reload()
        self.assertTrue(bucket.autoclass_enabled)
        self.assertIsNotNone(bucket.autoclass_toggle_time)
        self.assertEqual(bucket.autoclass_terminal_storage_class, "NEARLINE")
        blob = bucket.blob("cold")
        blob.storage_class = "COLDLINE"
        blob.upload_from_string(b"x", retry=None)
        self.assertEqual(blob.storage_class, "STANDARD")

    def test_project_service_account(self):
        self.assertTrue(self.client.get_service_account_email().endswith(
            "@gs-project-accounts.iam.gserviceaccount.com"))

    def test_project_hmac_keys(self):
        email = f"{unique('sdk-python')}@local-project.iam.gserviceaccount.com"
        if self.env.strict:
            # roles/storage.admin does not include storage.hmacKeys.*; strict mode enforces that.
            with self.assertRaises(exceptions.Forbidden):
                self.client.create_hmac_key(email, retry=None)
            return
        key, secret = self.client.create_hmac_key(email, retry=None)
        self.assertEqual(len(secret), 40)
        self.assertEqual(len(key.access_id), 61)
        self.assertEqual(key.state, "ACTIVE")
        self.assertEqual([item.access_id for item in self.client.list_hmac_keys(
            service_account_email=email)], [key.access_id])
        with self.assertRaises(exceptions.BadRequest):
            key.delete(retry=None)
        key.reload()
        self.assertEqual(key.state, "ACTIVE", "rejected delete changed the key")
        key.state = "INACTIVE"
        key.update(retry=None)
        self.assertEqual(key.state, "INACTIVE")
        key.delete(retry=None)
        key.reload()
        self.assertEqual(key.state, "DELETED")
        self.assertEqual(list(self.client.list_hmac_keys(service_account_email=email)), [])
        self.assertEqual([item.access_id for item in self.client.list_hmac_keys(
            service_account_email=email, show_deleted_keys=True)], [key.access_id])

    def test_uniform_bucket_level_access(self):
        def configure(item):
            item.iam_configuration.uniform_bucket_level_access_enabled = True
        bucket = self.new_bucket("sdk-python-ubla", configure)
        bucket.reload()
        self.assertTrue(bucket.iam_configuration.uniform_bucket_level_access_enabled)
        self.assertIsNotNone(bucket.iam_configuration.uniform_bucket_level_access_locked_time)
        bucket.iam_configuration.uniform_bucket_level_access_enabled = False
        bucket.patch()
        bucket.reload()
        self.assertFalse(bucket.iam_configuration.uniform_bucket_level_access_enabled)

    def test_lifecycle_rules(self):
        bucket = self.new_bucket("sdk-python-lifecycle", lambda item: item.add_lifecycle_delete_rule(age=30))
        bucket.reload()
        rules = list(bucket.lifecycle_rules)
        self.assertEqual(len(rules), 1)
        self.assertEqual((rules[0]["action"]["type"], rules[0]["condition"]["age"]), ("Delete", 30))
        bucket.clear_lifecycle_rules()
        bucket.add_lifecycle_delete_rule(age=60)
        bucket.add_lifecycle_set_storage_class_rule("COLDLINE", age=60)
        bucket.patch()
        bucket.reload()
        rules = list(bucket.lifecycle_rules)
        self.assertEqual(len(rules), 2)
        self.assertEqual(rules[-1]["action"], {"type": "SetStorageClass", "storageClass": "COLDLINE"})
        bucket.clear_lifecycle_rules()
        bucket.patch()
        bucket.reload()
        self.assertEqual(list(bucket.lifecycle_rules), [])

    def test_iam_test_permissions(self):
        bucket = self.new_bucket("sdk-python-permissions")
        permissions = ["storage.objects.get", "storage.objects.create", "storage.buckets.getIamPolicy"]
        self.assertEqual(sorted(bucket.test_iam_permissions(permissions)), sorted(permissions))
        if not self.env.strict:
            return
        try:
            self.env.session.token = "local-viewer-token"
            self.assertEqual(bucket.test_iam_permissions(permissions), ["storage.objects.get"])
            self.env.session.token = "local-bucket-reader-token"
            self.assertEqual(bucket.test_iam_permissions(permissions), [])
        finally:
            self.env.session.token = self.env.token


class ObjectJourneys(Journey):
    def test_default_content_type(self):
        bucket = self.new_bucket("sdk-python-default-type")
        blob = bucket.blob("extensionless")
        blob.upload_from_file(io.BytesIO(b"Python default bytes"), retry=None)
        self.assertEqual(blob.content_type, "application/octet-stream")
        self.assertEqual(blob.download_as_bytes(retry=None), b"Python default bytes")

    def test_holds_and_custom_time(self):
        bucket = self.new_bucket("sdk-python-holds")
        custom_time = datetime(2026, 10, 20, 1, 2, 3, tzinfo=timezone.utc)
        blob = bucket.blob("holds-custom-time")
        blob.custom_time = custom_time
        blob.upload_from_string(b"held bytes", retry=None)
        self.assertEqual(blob.custom_time, custom_time)
        blob.temporary_hold = True
        blob.patch()
        self.addCleanup(self._release_hold, blob)
        blob.reload()
        self.assertTrue(blob.temporary_hold)
        # A hold protects bytes while metadata remains editable.
        with self.assertRaises(exceptions.Forbidden):
            blob.delete()
        blob.temporary_hold = False
        blob.custom_time = custom_time + timedelta(days=1)
        blob.patch()
        blob.reload()
        self.assertFalse(blob.temporary_hold)
        self.assertEqual(blob.custom_time, custom_time + timedelta(days=1))

    @staticmethod
    def _release_hold(blob):
        blob.reload()
        if blob.temporary_hold:
            blob.temporary_hold = False
            blob.patch()

    def test_transfer_metadata_and_derived_objects(self):
        bucket = self.new_bucket("sdk-python-transfer")
        session = self.env.session
        with tempfile.TemporaryDirectory() as work:
            source = os.path.join(work, "source.bin")
            with open(source, "wb") as output:
                for _ in range(9):
                    output.write(bytes(range(256)) * 256)
            size = os.path.getsize(source)
            expected = _sha256(source)

            session.chunks = 0
            session.locations.clear()
            blob = bucket.blob("dir/日本語 +%?.bin")
            # upload_from_filename uses one multipart request up to 8 MiB even with a chunk
            # size; the public Blob.open("wb") writer always streams resumable chunks.
            with open(source, "rb") as data, blob.open(
                    "wb", chunk_size=CHUNK_SIZE, if_generation_match=0, retry=None) as output:
                shutil.copyfileobj(data, output, 64 * 1024)
            blob.reload()
            self.assertEqual(session.chunks, 3, "resumable chunks were not sent")
            self.assertEqual(len(session.locations), 1, "resumable Location missing")
            self.assertTrue(self.env.origin.matches(session.locations[0]),
                            "resumable Location was not the emulator origin")
            self.assertEqual(blob.size, size)

            self.assertEqual(bucket.get_blob(blob.name).generation, blob.generation)
            self.assertIn(blob.name, [item.name for item in bucket.list_blobs()])
            self.assertEqual([item.name for item in bucket.list_blobs(match_glob="**/*.bin")],
                             [blob.name])
            self.assertEqual(list(bucket.list_blobs(match_glob="missing-*")), [])

            blob.metadata = {"operation": "patch"}
            blob.patch()
            blob.reload()
            self.assertEqual(blob.metadata, {"operation": "patch"})
            # A resource holding only these fields makes Blob.update a replacement PUT.
            replacement = bucket.blob(blob.name)
            replacement.metadata = {"operation": "put"}
            replacement.content_type = "application/octet-stream"
            replacement.update(if_metageneration_match=blob.metageneration)
            blob.reload()
            self.assertEqual(blob.metadata, {"operation": "put"})

            # Bucket.copy_blob is objects.copy; Blob.rewrite and Blob.compose name their methods.
            copied = bucket.copy_blob(blob, bucket, "copy.bin", if_generation_match=0, retry=None)
            rewritten = bucket.blob("rewrite.bin")
            token, _, _ = rewritten.rewrite(blob, if_generation_match=0, retry=None)
            while token is not None:
                token, _, _ = rewritten.rewrite(blob, token=token, if_generation_match=0, retry=None)
            composed = bucket.blob("compose.bin")
            composed.compose([blob, rewritten], retry=None)
            for item in (copied, rewritten):
                item.reload()
                self.assertEqual(item.size, size)
            self.assertEqual(composed.size, 2 * size)

            digest = hashlib.sha256()
            for _ in range(2):
                with open(source, "rb") as part:
                    while chunk := part.read(64 * 1024):
                        digest.update(chunk)
            target = os.path.join(work, "download.bin")
            for item, want in ((blob, expected), (copied, expected), (rewritten, expected),
                               (composed, digest.hexdigest())):
                # The default "auto" checksum verifies the returned CRC32C.
                item.download_to_filename(target, retry=None)
                self.assertEqual(_sha256(target), want, f"download bytes mismatch: {item.name}")
            for item in (blob, copied, rewritten, composed):
                item.delete()

    def test_range_downloads(self):
        bucket = self.new_bucket("sdk-python-range")
        blob = self.upload(bucket, "range.txt", b"0123456789")
        self.assertEqual(blob.download_as_bytes(start=2, end=5, retry=None), b"2345")
        self.assertEqual(blob.download_as_bytes(start=7, retry=None), b"789")
        self.assertEqual(blob.download_as_bytes(start=-3, retry=None), b"789")

    def test_move(self):
        bucket = self.new_bucket("sdk-python-move")
        source = self.upload(bucket, "source", b"Python move bytes")
        moved = bucket.move_blob(source, "destination", retry=None)
        self.assertEqual(moved.name, "destination")
        self.assertIsNone(bucket.get_blob("source"))
        self.assertEqual(bucket.blob("destination").download_as_bytes(retry=None), b"Python move bytes")


class ProtectionJourneys(Journey):
    def test_soft_delete_restore(self):
        bucket = self.new_bucket("sdk-python-soft-delete")
        bucket.soft_delete_policy.retention_duration_seconds = 604800
        bucket.patch()
        bucket.reload()
        self.assertEqual(bucket.soft_delete_policy.retention_duration_seconds, 604800)
        original = self.upload(bucket, "recoverable", b"recoverable Python bytes")
        generation = original.generation
        original.delete()
        self.assertIsNone(bucket.get_blob("recoverable"))
        deleted = list(bucket.list_blobs(soft_deleted=True))
        self.assertEqual([item.generation for item in deleted], [generation])
        self.assertIsNotNone(bucket.get_blob("recoverable", generation=generation, soft_deleted=True))
        restored = bucket.restore_blob("recoverable", generation=generation, retry=None)
        self.assertGreater(restored.generation, generation)
        self.assertEqual(restored.metageneration, 1)
        self.assertEqual(restored.download_as_bytes(retry=None), b"recoverable Python bytes")

    def test_bucket_retention_policy(self):
        # Retention locks are tested only on an empty emulator bucket.
        bucket = self.client.create_bucket(unique("sdk-python-retention"), retry=None)
        self.addCleanup(bucket.delete)
        bucket.retention_period = 3600
        bucket.patch()
        bucket.reload()
        self.assertEqual(bucket.retention_period, 3600)
        retained = self.upload(bucket, "retained", b"retained bytes")
        with self.assertRaises(exceptions.Forbidden):
            retained.delete()
        # The unlocked policy must be removed before cleaning up this object.
        bucket.retention_period = None
        bucket.patch()
        retained.delete()
        bucket.retention_period = 7200
        bucket.patch()
        bucket.reload()
        bucket.lock_retention_policy(retry=None)
        bucket.reload()
        self.assertTrue(bucket.retention_policy_locked)
        bucket.retention_period = 3600
        with self.assertRaises(exceptions.BadRequest) as raised:
            bucket.patch()
        message = raised.exception.message
        self.assertIn("locked", message)
        self.assertIn("extend", message)


def _sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        while chunk := source.read(64 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


if __name__ == "__main__":
    unittest.main()
