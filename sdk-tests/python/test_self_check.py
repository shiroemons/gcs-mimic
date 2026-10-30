"""Offline checks of the origin validation and the transport guard; no server is needed."""

from pathlib import Path
import ssl
import unittest

import requests

from harness import GuardedSession, Origin, trust_context

DEVELOPMENT_CA = Path(__file__).resolve().parents[2] / "src/tls/development-ca.pem"


class OriginSelfCheck(unittest.TestCase):
    def test_accepts_loopback_origins(self):
        self.assertEqual(Origin("https://127.0.0.1:4443").port, 4443)
        self.assertEqual(Origin("https://localhost:4443").hostname, "localhost")
        self.assertEqual(Origin("http://127.0.0.1:4443").scheme, "http")
        self.assertEqual(Origin("http://[::1]:4443").hostname, "::1")
        self.assertEqual(Origin("http://127.0.0.1:4443/").url, "http://127.0.0.1:4443")

    def test_rejects_unsafe_origins(self):
        for raw in ("https://storage.googleapis.com", "http://storage.googleapis.com",
                    "https://169.254.169.254", "http://metadata.google.internal",
                    "ftp://127.0.0.1", "https://127.0.0.1.example.invalid",
                    "https://user@127.0.0.1", "https://127.0.0.1/storage/v1",
                    "https://127.0.0.1?cloud=1", "https://127.0.0.1#fragment"):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                Origin(raw)


class _NoNetworkAdapter(requests.adapters.BaseAdapter):
    """Answers every request locally so the guard can be exercised without a server."""

    def send(self, request, **kwargs):
        response = requests.Response()
        response.status_code = 200
        response.url = request.url
        response.request = request
        return response

    def close(self):
        pass


class TrustSelfCheck(unittest.TestCase):
    def test_trust_context_pins_the_ca_and_keeps_verification(self):
        context = trust_context(str(DEVELOPMENT_CA))
        self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
        self.assertTrue(context.check_hostname)
        self.assertTrue(context.verify_flags & ssl.VERIFY_X509_STRICT)
        # Only the supplied CA is trusted; the system store is not loaded.
        self.assertEqual(len(context.get_ca_certs()), 1)


class GuardSelfCheck(unittest.TestCase):
    def test_guard_allows_only_the_exact_origin(self):
        for scheme in ("http", "https"):
            other_scheme = "https" if scheme == "http" else "http"
            session = GuardedSession(Origin(f"{scheme}://127.0.0.1:4443"))
            session.mount("http://", _NoNetworkAdapter())
            session.mount("https://", _NoNetworkAdapter())
            self.assertEqual(session.get(f"{scheme}://127.0.0.1:4443/").status_code, 200)
            for raw in ("https://storage.googleapis.com/", "http://169.254.169.254/",
                        "http://metadata.google.internal/", "https://127.0.0.1:4444/",
                        f"{other_scheme}://127.0.0.1:4443/"):
                with self.subTest(scheme=scheme, raw=raw), self.assertRaisesRegex(
                        ValueError, "outside the emulator origin"):
                    session.get(raw)

    def test_guard_attaches_only_the_configured_token(self):
        origin = Origin("http://127.0.0.1:4443")
        for token, expected in ((None, None), ("local-admin-token", "Bearer local-admin-token")):
            session = GuardedSession(origin, token=token)
            session.mount("http://", _NoNetworkAdapter())
            response = session.get("http://127.0.0.1:4443/")
            self.assertEqual(response.request.headers.get("Authorization"), expected)
            self.assertFalse(session.trust_env)


if __name__ == "__main__":
    unittest.main()
