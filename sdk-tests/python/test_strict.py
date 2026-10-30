"""Strict-profile authorization and signed URL journeys; requires GCS_MIMIC_TOKEN."""

from datetime import timedelta
from pathlib import Path
import unittest
from urllib.parse import parse_qs, urlsplit

from google.api_core import exceptions
from google.oauth2 import service_account

from harness import delete_bucket, environment, unique

ISSUER = "local-admin@example.invalid"
KEY = Path(__file__).resolve().parents[2] / "fixtures/signing/test-only-private.pem"
BODY = b"Python signed private bytes"
RESPONSE_LIMIT = 64 * 1024


def setUpModule():
    if not environment().strict:
        # Fail rather than skip, so a misconfigured strict run cannot pass silently.
        raise RuntimeError("strict journeys require GCS_MIMIC_TOKEN")


class StrictJourney(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.env = environment()
        cls.client = cls.env.client

    def setUp(self):
        self.bucket = self.client.create_bucket(unique("sdk-python-strict"), retry=None)
        self.addCleanup(delete_bucket, self.bucket)


class StrictAuthorization(StrictJourney):
    def test_denies_missing_and_insufficient_principals(self):
        try:
            self.env.session.token = None
            with self.assertRaises(exceptions.Unauthorized):
                self.client.get_bucket(self.bucket.name)
            # objectViewer may read but not create objects.
            self.env.session.token = "local-viewer-token"
            with self.assertRaises(exceptions.Forbidden):
                self.bucket.blob("denied.txt").upload_from_string(b"denied", retry=None)
        finally:
            self.env.session.token = self.env.token
        self.assertIsNone(self.bucket.get_blob("denied.txt"), "denied upload left an object")


class SignedUrls(StrictJourney):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        # A separate offline signer: the fixture key is public test material, and signing is
        # local, so google-auth never contacts token_uri.
        cls.signer = service_account.Credentials.from_service_account_info({
            "type": "service_account", "client_email": ISSUER,
            "private_key": KEY.read_text(), "token_uri": "https://oauth2.googleapis.com/token",
        })
        cls.http = cls.env.unsigned_session()

    def sign(self, blob, method, **kwargs):
        return blob.generate_signed_url(expiration=timedelta(minutes=5), method=method,
                                        credentials=self.signer, **kwargs)

    def call(self, method, url, content_type, expected_status, host=None):
        """Sends one request without a bearer token and returns its bounded body."""
        headers = {}
        if content_type is not None:
            headers["Content-Type"] = content_type
        if host is not None:
            headers["Host"] = host
        response = self.http.request(method, url, headers=headers,
                                     data=BODY if method == "PUT" else None, stream=True)
        with response:
            body = b""
            for chunk in response.iter_content(16 * 1024):
                body += chunk
                self.assertLessEqual(len(body), RESPONSE_LIMIT, "signed response exceeded test limit")
        self.assertEqual(response.status_code, expected_status, f"signed {method} {url}: {body[:300]!r}")
        if expected_status == 403:
            self.assertIn(b"<Code>SignatureDoesNotMatch</Code>", body)
        return response, body

    def assert_endpoint(self, url):
        parts = urlsplit(url)
        self.assertEqual((parts.scheme, parts.netloc), (self.env.origin.scheme, self.env.origin.netloc))
        self.assertEqual(parts.path, f"/{self.bucket.name}/signed.txt")

    def test_v4_get_put_with_signed_content_type(self):
        blob = self.bucket.blob("signed.txt")
        signed_type = {"Content-Type": "text/plain"}
        put_url = self.sign(blob, "PUT", version="v4", headers=signed_type)
        get_url = self.sign(blob, "GET", version="v4", headers=signed_type)
        # The SDK binds the client endpoint's host:port, so no URL adaptation is needed.
        for url in (put_url, get_url):
            self.assert_endpoint(url)
            self.assertEqual(parse_qs(urlsplit(url).query)["X-Goog-SignedHeaders"], ["content-type;host"])
        self.call("PUT", put_url, "text/plain", 200)
        self.call("PUT", put_url, "changed", 403)
        self.call("PUT", put_url, None, 403)
        self.call("PUT", f"{put_url}&tampered=1", "text/plain", 403)
        self.call("PUT", put_url.replace("/signed.txt?", "/changed.txt?"), "text/plain", 403)
        self.call("PUT", put_url, "text/plain", 403, host="changed.invalid")
        self.call("GET", put_url, "text/plain", 403)
        self.call("PUT", get_url, "text/plain", 403)
        _, body = self.call("GET", get_url, "text/plain", 200)
        self.assertEqual(body, BODY)
        self.assertEqual(self.bucket.get_blob("signed.txt").content_type, "text/plain")
        self.assertIsNone(self.bucket.get_blob("changed.txt"))
        # The object stays private: an unsigned anonymous read is rejected.
        self.call("GET", f"{self.env.origin.url}/{self.bucket.name}/signed.txt", None, 401)

    def test_v4_response_overrides(self):
        blob = self.bucket.blob("signed.txt")
        blob.upload_from_string(BODY, content_type="text/plain", retry=None)
        disposition = 'attachment; filename="Python + file.csv"'
        url = self.sign(blob, "GET", version="v4", headers={"Content-Type": "text/plain"},
                        response_disposition=disposition, response_type="text/csv")
        response, body = self.call("GET", url, "text/plain", 200)
        self.assertEqual(body, BODY)
        self.assertEqual(response.headers["Content-Disposition"], disposition)
        self.assertEqual(response.headers["Content-Type"], "text/csv")
        changed = url.replace("text%2Fcsv", "text%2Fhtml")
        self.assertNotEqual(changed, url, "override tamper did not change the URL")
        self.call("GET", changed, "text/plain", 403)
        self.assertEqual(self.bucket.get_blob("signed.txt").content_type, "text/plain",
                         "response override mutated metadata")

    def test_v2_default_and_explicit_get_put(self):
        blob = self.bucket.blob("signed.txt")
        blob.upload_from_string(BODY, content_type="text/plain", retry=None)
        for version in (None, "v2"):
            with self.subTest(version=version):
                options = {} if version is None else {"version": version}
                get_url = self.sign(blob, "GET", **options)
                self.assert_endpoint(get_url)
                self.assertIn("GoogleAccessId=", get_url)
                _, body = self.call("GET", get_url, None, 200)
                self.assertEqual(body, BODY)
                put_url = self.sign(blob, "PUT", content_type="text/plain", **options)
                self.call("PUT", put_url, "changed", 403)
                self.call("PUT", put_url, "text/plain", 200)
                self.call("GET", put_url, "text/plain", 403)
                self.call("GET", get_url.replace("/signed.txt?", "/changed.txt?"), None, 403)


if __name__ == "__main__":
    unittest.main()
