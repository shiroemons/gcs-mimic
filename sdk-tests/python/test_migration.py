"""The anonymous migration journey against the runner's preinitialized `assets` bucket."""

import unittest

from harness import environment

NAME = "uploads/python-migration.txt"
PAYLOAD = b"Python migration bytes\n"


class MigrationScenario(unittest.TestCase):
    def test_initialized_bucket_upload_get_download_delete(self):
        env = environment()
        self.assertFalse(env.strict, "the migration scenario requires an anonymous client")
        bucket = env.client.get_bucket("assets")
        self.assertIsNone(bucket.get_blob(NAME), "migration object already exists")
        blob = bucket.blob(NAME)
        # No content_type: upload_from_string sends the SDK's text/plain default. This SDK
        # infers types only from a local filename, never from the object name.
        blob.upload_from_string(PAYLOAD, retry=None)
        self.addCleanup(lambda: bucket.get_blob(NAME) and bucket.delete_blob(NAME))
        found = bucket.get_blob(NAME)
        self.assertIsNotNone(found)
        self.assertEqual((found.generation, found.size, found.content_type),
                         (blob.generation, len(PAYLOAD), "text/plain"))
        self.assertEqual(found.download_as_bytes(retry=None), PAYLOAD)
        found.delete()
        self.assertIsNone(bucket.get_blob(NAME), "deleted migration object still exists")


if __name__ == "__main__":
    unittest.main()
