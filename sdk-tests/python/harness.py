"""Hermetic transport and client construction for the official Python SDK suite."""

import ipaddress
import os
import secrets
import socket
import ssl
from urllib.parse import urlsplit

from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
import requests
from requests.adapters import HTTPAdapter

PROJECT = "local-project"
REQUEST_TIMEOUT = 10
CHUNK_SIZE = 256 * 1024
_DEFAULT_PORTS = {"http": 80, "https": 443}

# Proxy and credential-discovery inputs are removed before the SDK is used, so neither
# requests nor google-auth can be steered away from the explicit local configuration.
_ISOLATED_ENVIRONMENT = (
    "http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY", "all_proxy", "ALL_PROXY",
    "no_proxy", "NO_PROXY", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE",
    "GOOGLE_APPLICATION_CREDENTIALS", "GOOGLE_CLOUD_PROJECT", "GCLOUD_PROJECT",
    "API_ENDPOINT_OVERRIDE", "GOOGLE_API_USE_CLIENT_CERTIFICATE", "CLOUDSDK_CONFIG",
)


class Origin:
    """A validated loopback HTTP or HTTPS origin, compared by scheme, host and port."""

    def __init__(self, raw):
        try:
            parts = urlsplit(raw)
            port = parts.port
        except ValueError as error:
            raise ValueError("invalid local HTTP or HTTPS emulator origin") from error
        if (parts.scheme not in ("http", "https") or parts.username is not None
                or parts.password is not None or parts.path not in ("", "/")
                or parts.query or parts.fragment or "?" in raw or "#" in raw
                or not _is_loopback(parts.hostname)):
            raise ValueError("GCS_MIMIC_ENDPOINT must be a loopback HTTP or HTTPS origin")
        self.scheme = parts.scheme
        self.hostname = parts.hostname
        self.port = port or _DEFAULT_PORTS[parts.scheme]
        self.netloc = parts.netloc
        self.url = f"{parts.scheme}://{parts.netloc}"

    def matches(self, url):
        parts = urlsplit(url)
        port = parts.port or _DEFAULT_PORTS.get(parts.scheme)
        return (parts.scheme, parts.hostname, port) == (self.scheme, self.hostname, self.port)


def _is_loopback(hostname):
    if hostname is None:
        return False
    if hostname == "localhost":
        return True
    try:
        return ipaddress.ip_address(hostname).is_loopback
    except ValueError:
        return False


def require_localhost_loopback(origin):
    """Rejects a localhost origin whose resolver answers include a non-loopback address."""
    if origin.hostname != "localhost":
        return
    addresses = {info[4][0] for info in socket.getaddrinfo("localhost", origin.port)}
    if not addresses or not all(ipaddress.ip_address(address.split("%")[0]).is_loopback
                                for address in addresses):
        raise ValueError(f"localhost resolves outside loopback: {sorted(addresses)}")


def trust_context(ca_cert):
    """Builds an SSL context that trusts only ca_cert and verifies the peer and hostname.

    Python 3.13+ enables VERIFY_X509_STRICT by default, which rejects the emulator's built-in
    development leaf certificate because it lacks an Authority Key Identifier extension. Only
    that RFC 5280 strictness flag is cleared; chain, expiry and hostname checks stay enabled.
    """
    context = ssl.create_default_context(cafile=ca_cert)
    context.verify_flags &= ~ssl.VERIFY_X509_STRICT
    return context


class _TrustAdapter(HTTPAdapter):
    def __init__(self, context):
        self._context = context
        super().__init__()

    def init_poolmanager(self, *args, **kwargs):
        kwargs["ssl_context"] = self._context
        return super().init_poolmanager(*args, **kwargs)


class GuardedSession(requests.Session):
    """A requests session that refuses any request outside one emulator origin.

    Environment proxies and redirects are disabled, every request has a fixed timeout and
    an optional static bearer token is attached here instead of through google-auth, so the
    SDK never performs credential discovery or token refresh.
    """

    def __init__(self, origin, ca_cert=None, token=None):
        super().__init__()
        self.origin = origin
        self.trust_env = False
        self.verify = True
        if origin.scheme == "https" and ca_cert is not None:
            self.verify = ca_cert
            self.mount("https://", _TrustAdapter(trust_context(ca_cert)))
        self.token = token
        self.chunks = 0
        self.locations = []

    def send(self, request, **kwargs):
        if not self.origin.matches(request.url):
            raise ValueError("refusing request outside the emulator origin")
        if self.token is not None:
            request.headers["Authorization"] = f"Bearer {self.token}"
        if request.method == "PUT" and "Content-Range" in request.headers:
            self.chunks += 1
        kwargs["timeout"] = REQUEST_TIMEOUT
        kwargs["allow_redirects"] = False
        kwargs["proxies"] = {}
        response = super().send(request, **kwargs)
        if "Location" in response.headers:
            self.locations.append(response.headers["Location"])
        return response


class Environment:
    """The emulator origin, trust input, optional token and SDK client for one run."""

    def __init__(self):
        for key in _ISOLATED_ENVIRONMENT:
            os.environ.pop(key, None)
        self.origin = Origin(os.environ["GCS_MIMIC_ENDPOINT"])
        require_localhost_loopback(self.origin)
        self.ca_cert = None
        if self.origin.scheme == "https":
            self.ca_cert = os.environ["GCS_MIMIC_CA_CERT"]
            if not (os.path.isfile(self.ca_cert) and os.access(self.ca_cert, os.R_OK)):
                raise ValueError("CA certificate is unreadable")
        self.token = os.environ.get("GCS_MIMIC_TOKEN")
        self.strict = self.token is not None
        self.session = GuardedSession(self.origin, self.ca_cert, self.token)
        # The SDK documents STORAGE_EMULATOR_HOST as scheme-inclusive. Explicit anonymous
        # credentials and project keep google-auth from consulting ADC or the metadata server.
        os.environ["STORAGE_EMULATOR_HOST"] = self.origin.url
        self.client = storage.Client(
            project=PROJECT, credentials=AnonymousCredentials(), _http=self.session,
            client_options={"api_endpoint": self.origin.url})

    def unsigned_session(self):
        """A guarded session without the bearer token, for signed and anonymous requests."""
        return GuardedSession(self.origin, self.ca_cert)


_ENVIRONMENT = None


def environment():
    global _ENVIRONMENT
    if _ENVIRONMENT is None:
        _ENVIRONMENT = Environment()
    return _ENVIRONMENT


def unique(prefix):
    return f"{prefix}-{secrets.token_hex(6)}"


def delete_bucket(bucket):
    """Deletes every live object version and then the bucket; holds must already be released."""
    for blob in bucket.list_blobs(versions=True):
        bucket.delete_blob(blob.name, generation=blob.generation, retry=None)
    bucket.delete(retry=None)
