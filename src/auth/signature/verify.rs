//! RSA verification uses an Auth-owned bounded blocking budget, never a catalog transaction.
//! Bounded HMAC chains for streaming seeds and individual chunks run inline.

use super::*;
use crate::{
    auth::{Profile, Registry, SigningKeys, test_signer},
    bucket::CatalogError,
    storage::{Catalog, failure::BackendCause},
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Semaphore;

#[derive(Clone, Debug)]
pub(crate) struct Verifiers(Arc<Semaphore>);

impl Verifiers {
    pub(crate) fn for_blocking_threads(threads: usize) -> Self {
        Self::new(NonZeroUsize::new(threads / 2).unwrap_or(NonZeroUsize::MIN))
    }

    fn new(permits: NonZeroUsize) -> Self {
        Self(Arc::new(Semaphore::new(permits.get())))
    }

    pub(crate) async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, VerificationError> {
        let permit = Arc::clone(&self.0)
            .acquire_owned()
            .await
            .map_err(|error| VerificationError::Worker(BackendCause::from_error(error)))?;
        crate::task::run_blocking("signature-verification", move || {
            let _permit = permit;
            job()
        })
        .await
        .map_err(|error| VerificationError::Worker(BackendCause::from_error(error)))
    }
}

impl Default for Verifiers {
    fn default() -> Self {
        Self::new(NonZeroUsize::MIN)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Algorithm {
    Rsa,
    GoogleHmac,
    AwsHmac,
    LegacyHmacSha1,
}

pub(super) struct Prepared {
    pub signer: String,
    pub date: String,
    pub algorithm: Algorithm,
    pub scope: String,
    pub message: String,
    /// For a V4 signature over a Host that names an explicit port, the string to sign with that
    /// port removed from the canonical host. Signatures that do not cover Host have none.
    pub portless: Option<String>,
    pub signature: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum VerificationError {
    Signature(Failure),
    Catalog(CatalogError),
    Worker(BackendCause),
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signature(_) => f.write_str("Invalid XML signature"),
            Self::Catalog(_) | Self::Worker(_) => f.write_str("Signature verifier unavailable"),
        }
    }
}

impl std::error::Error for VerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Signature(_) => None,
            Self::Catalog(error) => Some(error),
            Self::Worker(error) => Some(error),
        }
    }
}

impl From<Failure> for VerificationError {
    fn from(value: Failure) -> Self {
        Self::Signature(value)
    }
}

pub(super) enum Key {
    Rsa(Arc<[PublicKey]>),
    Hmac(String),
}

/// Where a signed request's RSA keys come from, and whom a verified signature makes the caller.
pub(super) enum Signers<'a> {
    /// Strict: the signer must be a registered identity, which becomes the caller.
    Registered(&'a Registry),
    /// Permissive `signed_urls=verify`: any known key verifies, and access stays permissive,
    /// because an unsigned request is not authorized by identity either.
    Keys(&'a SigningKeys),
}

impl<'a> Signers<'a> {
    pub(super) fn keys(&self) -> &'a SigningKeys {
        match self {
            Self::Registered(registry) => &registry.signing_keys,
            Self::Keys(keys) => keys,
        }
    }

    fn caller(&self, signer: &str) -> Result<Option<Caller<'a>>, Failure> {
        match self {
            Self::Registered(registry) => registry
                .identity(signer)
                .map(|identity| Some(Caller::Principal(identity)))
                .ok_or_else(|| self.unknown(signer)),
            Self::Keys(_) => Ok(None),
        }
    }

    /// Without the built-in key the signer's own ID is unknown, which says what to enable;
    /// otherwise the failure names where this profile looks for keys.
    fn unknown(&self, signer: &str) -> Failure {
        Failure::UnknownSigner(
            if signer == test_signer::ID && !self.keys().contains(signer) {
                KeyLookup::DisabledTestSigner
            } else {
                match self {
                    Self::Registered(_) => KeyLookup::Registry,
                    Self::Keys(_) => KeyLookup::PermissiveKeys,
                }
            },
        )
    }
}

pub(crate) fn has_header(headers: &HeaderMap) -> bool {
    v4::has_header(headers) || v2::has_header(headers)
}

impl Auth {
    /// `None` when XML signing credentials are ignored, as in default permissive mode.
    pub(super) fn signers(&self) -> Option<Signers<'_>> {
        match &self.profile {
            Profile::Permissive => None,
            Profile::VerifyingPermissive(keys) => Some(Signers::Keys(keys)),
            Profile::Strict(registry) => Some(Signers::Registered(registry)),
        }
    }

    pub(crate) async fn authenticate_xml_resource<'a>(
        &'a self,
        catalog: &Catalog,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        clock: &dyn Clock,
        resource: &Resource,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        enum SignatureKind {
            V4,
            V2Query,
            V2Header,
        }
        let kind = if v2::is_v2(uri.query()) {
            SignatureKind::V2Query
        } else if v2::has_header(headers) {
            SignatureKind::V2Header
        } else {
            SignatureKind::V4
        };
        if headers
            .get("x-amz-content-sha256")
            .is_some_and(|value| value == STREAMING_PAYLOAD)
        {
            // Only the ordinary PUT adapter carries the authenticated chunk-verifier state.
            return Err(Failure::Malformed.into());
        }
        let Some(signers) = self.signers() else {
            // Preserve historical V2 shape checks; permissive V4 credentials are ignored.
            if matches!(kind, SignatureKind::V2Query) {
                v2::validate_query_shape(uri)?;
            }
            return Ok(None);
        };
        v4::bounded(uri, headers)?;
        let prepared = if matches!(kind, SignatureKind::V2Query | SignatureKind::V2Header) {
            let mut prepared = v2::prepare_resource(method, uri, headers, clock.now(), resource)?;
            prepared.algorithm = if matches!(kind, SignatureKind::V2Query)
                && signers.keys().contains(&prepared.signer)
            {
                Algorithm::Rsa
            } else {
                Algorithm::LegacyHmacSha1
            };
            prepared
        } else {
            v4::prepare(method, uri, headers, clock.now())?
        };
        self.verify_prepared(catalog, prepared).await
    }

    /// The caller is `None` when a verified signature leaves access permissive.
    pub(super) async fn verify_prepared<'a>(
        &'a self,
        catalog: &Catalog,
        prepared: v4::Prepared,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        let (caller, key) = self.verification_key(catalog, &prepared).await?;
        let host_port = self.signed_host_port;
        self.verifiers
            .run(move || {
                verify_host_forms(host_port, &prepared, |message| {
                    verify(&key, &prepared, message)
                })
            })
            .await??;
        Ok(caller)
    }

    /// RSA keys come from the active signers; HMAC keys from the catalog, whose ACTIVE key
    /// alone is enough when a verified signature authorizes nothing by identity.
    pub(super) async fn verification_key<'a>(
        &'a self,
        catalog: &Catalog,
        prepared: &Prepared,
    ) -> Result<(Option<Caller<'a>>, Key), VerificationError> {
        // Only a signed aws-chunked upload asks for a key while signatures are ignored.
        let signers = self
            .signers()
            .ok_or(Failure::UnknownSigner(KeyLookup::Ignored))?;
        if prepared.algorithm == v4::Algorithm::Rsa {
            let keys = signers
                .keys()
                .get(&prepared.signer)
                .ok_or_else(|| signers.unknown(&prepared.signer))?;
            return Ok((
                signers.caller(&prepared.signer)?,
                Key::Rsa(Arc::clone(keys)),
            ));
        }
        let (signer, secret) = catalog
            .active_hmac_key(&prepared.signer)
            .await
            .map_err(VerificationError::Catalog)?
            .ok_or_else(|| signers.unknown(&prepared.signer))?;
        Ok((signers.caller(&signer)?, Key::Hmac(secret)))
    }
}

/// Process-wide, so an operator notices a lenient match without one line per request.
static WARNED_PORTLESS_HOST: AtomicBool = AtomicBool::new(false);

/// Checks a signature over the canonical request as sent and, when that fails and the request
/// Host names an explicit port, once more over the same request without that port. A lenient
/// verifier accepts that match; an exact one reports it, so the hint never accompanies a
/// signature that fails for another reason. Every other part of the canonical request stays
/// bound either way, and the second check runs only for a request that already failed.
pub(super) fn verify_host_forms(
    host_port: SignedHostPort,
    prepared: &Prepared,
    signs: impl Fn(&str) -> bool,
) -> Result<(), Failure> {
    if signs(&prepared.message) {
        return Ok(());
    }
    let Some(portless) = &prepared.portless else {
        return Err(Failure::Mismatch);
    };
    if !signs(portless) {
        return Err(Failure::Mismatch);
    }
    match host_port {
        SignedHostPort::Exact => Err(Failure::MismatchWithHostPort),
        SignedHostPort::Lenient => {
            warn_portless_host();
            Ok(())
        }
    }
}

fn warn_portless_host() {
    if !WARNED_PORTLESS_HOST.swap(true, Ordering::Relaxed) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "signature_host_port_omitted",
            serde_json::json!({
                "message": "a V4 signature matched only without the port of the request Host because signed_host_port=lenient; its signer omitted the port, as Go storage does. Sign the final host and port, for example with gcs-mimic url sign, to verify it exactly; this warning is logged once per process",
            }),
        );
    }
}

fn verify(key: &Key, prepared: &v4::Prepared, message: &str) -> bool {
    match key {
        Key::Rsa(keys) => keys
            .iter()
            .any(|key| key.verify(message.as_bytes(), &prepared.signature)),
        Key::Hmac(secret) => {
            // The legacy V2 formats use SHA-1 with the literal, underived UTF-8 secret.
            if prepared.algorithm == Algorithm::LegacyHmacSha1 {
                let key = ring::hmac::Key::new(
                    ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
                    secret.as_bytes(),
                );
                return ring::hmac::verify(&key, message.as_bytes(), &prepared.signature).is_ok();
            }
            let prefix = match prepared.algorithm {
                Algorithm::GoogleHmac => "GOOG4",
                Algorithm::AwsHmac => "AWS4",
                Algorithm::Rsa | Algorithm::LegacyHmacSha1 => return false,
            };
            // The API returns a base64-looking secret, but the V4 input is its literal UTF-8.
            let mut key = ring::hmac::Key::new(
                ring::hmac::HMAC_SHA256,
                format!("{prefix}{secret}").as_bytes(),
            );
            for component in prepared.scope.split('/') {
                let tag = ring::hmac::sign(&key, component.as_bytes());
                key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, tag.as_ref());
            }
            ring::hmac::verify(&key, message.as_bytes(), &prepared.signature).is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the string to sign over the request as sent, which the published vectors cover.
    fn verify(key: Key, prepared: &Prepared) -> bool {
        super::verify(&key, prepared, &prepared.message)
    }

    #[tokio::test]
    async fn verifier_budgets_bound_jobs_and_do_not_share_a_global_queue() {
        let first = Verifiers::for_blocking_threads(2);
        let second = Verifiers::for_blocking_threads(4);
        let held = first.0.acquire().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), first.run(|| ()))
                .await
                .is_err()
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), second.run(|| 42))
                .await
                .unwrap()
                .unwrap(),
            42
        );
        drop(held);
        first.run(|| ()).await.unwrap();
        let (started, mut ready) = tokio::sync::mpsc::unbounded_channel();
        let (release_a, wait_a) = std::sync::mpsc::channel();
        let (release_b, wait_b) = std::sync::mpsc::channel();
        let other = started.clone();
        let a = second.run(move || {
            started.send(()).unwrap();
            wait_a.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let b = second.run(move || {
            other.send(()).unwrap();
            wait_b.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let release = async {
            for _ in 0..2 {
                tokio::time::timeout(Duration::from_secs(1), ready.recv())
                    .await
                    .unwrap()
                    .unwrap();
            }
            release_a.send(()).unwrap();
            release_b.send(()).unwrap();
        };
        let (a, b, ()) = tokio::join!(a, b, release);
        a.unwrap();
        b.unwrap();
    }

    #[tokio::test]
    async fn legacy_hmac_lookup_failures_keep_the_catalog_cause_and_xml_503() {
        use std::error::Error as _;
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.close().await.unwrap();
        let auth = Auth::strict(
            crate::auth::Registry::parse(
                br#"{"identities":[{"id":"signer@example.invalid","tokens":[]}]}"#,
            )
            .unwrap(),
        );
        let clock = crate::clock::SystemClock;
        let now = clock.now();
        let expires = (now + Duration::from_secs(3600))
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for kind in ["query", "GOOG1", "AWS"] {
            let mut headers = HeaderMap::new();
            headers.insert("host", "localhost".parse().unwrap());
            let uri: Uri = if kind == "query" {
                format!("/bucket/object?GoogleAccessId=key&Expires={expires}&Signature=AAAAAAAAAAAAAAAAAAAAAAAAAAA%3D").parse().unwrap()
            } else {
                headers.insert("date", httpdate::fmt_http_date(now).parse().unwrap());
                headers.insert(
                    "authorization",
                    format!("{kind} key:AAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                        .parse()
                        .unwrap(),
                );
                "/bucket/object".parse().unwrap()
            };
            let failure = auth
                .authenticate_xml_resource(
                    &catalog,
                    &Method::GET,
                    &uri,
                    &headers,
                    &clock,
                    &Resource::PathStyle,
                )
                .await
                .unwrap_err();
            assert!(matches!(failure, VerificationError::Catalog(_)));
            assert!(failure.source().is_some());
            let xml = crate::xml::authenticate_target(crate::xml::Authentication {
                auth: &auth,
                catalog: &catalog,
                method: &Method::GET,
                uri: &uri,
                headers: &headers,
                clock: &clock,
                resource: &Resource::PathStyle,
            })
            .await
            .unwrap_err();
            assert_eq!(xml.status(), hyper::StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[test]
    fn published_legacy_header_vector_uses_raw_date_and_virtual_bucket_resource() {
        let mut headers = HeaderMap::new();
        headers.insert("date", "Tue, 27 Mar 2007 19:36:42 +0000".parse().unwrap());
        headers.insert(
            "authorization",
            "AWS AKIAIOSFODNN7EXAMPLE:qgk2+6Sv9/oM7G3qLEjTH1a1l1g="
                .parse()
                .unwrap(),
        );
        let prepared = v2::prepare_resource(
            &Method::GET,
            &"/photos/puppy.jpg".parse().unwrap(),
            &headers,
            parse_timestamp("20070327T193642Z").unwrap(),
            &Resource::VirtualHosted("awsexamplebucket1".into()),
        )
        .unwrap();
        assert_eq!(
            prepared.message,
            "GET\n\n\nTue, 27 Mar 2007 19:36:42 +0000\n/awsexamplebucket1/photos/puppy.jpg"
        );
        assert!(verify(
            Key::Hmac("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into()),
            &prepared
        ));
    }

    #[test]
    fn independent_legacy_query_vector_reconciles_the_published_bucket_mismatch() {
        let uri: Uri = "/awsexamplebucket1/photos/puppy.jpg?GoogleAccessId=AKIAIOSFODNN7EXAMPLE&Expires=1175139620&Signature=NpgCjnDzrM%2BWFzoENXmpNDUsSn8%3D".parse().unwrap();
        assert!(
            v2::prepare(
                &Method::GET,
                &uri,
                &HeaderMap::new(),
                UNIX_EPOCH + Duration::from_secs(1175139619)
            )
            .is_ok()
        );
        for seconds in [1175139620, 1175139621] {
            assert!(matches!(
                v2::prepare(
                    &Method::GET,
                    &uri,
                    &HeaderMap::new(),
                    UNIX_EPOCH + Duration::from_secs(seconds)
                ),
                Err(Failure::Expired)
            ));
        }
        let mut prepared = v2::prepare(
            &Method::GET,
            &uri,
            &HeaderMap::new(),
            UNIX_EPOCH + Duration::from_secs(1175139600),
        )
        .unwrap();
        prepared.algorithm = Algorithm::LegacyHmacSha1;
        assert_eq!(
            prepared.message,
            "GET\n\n\n1175139620\n/awsexamplebucket1/photos/puppy.jpg"
        );
        // The archive changed the bucket but retained the older bucket's query signature.
        assert!(!verify(
            Key::Hmac("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into()),
            &prepared
        ));
        // Independently generated with OpenSSL against the archive's literal new-bucket input.
        prepared.signature = v2::decode_signature("1No4mq5ETf02z8aet9voy6gui6E=").unwrap();
        assert!(verify(
            Key::Hmac("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into()),
            &prepared
        ));
        prepared.message.push('x');
        assert!(!verify(
            Key::Hmac("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into()),
            &prepared
        ));
    }

    #[test]
    fn legacy_policy_hmac_sha1_matches_rfc2202_without_v4_derivation() {
        let mut prepared = Prepared {
            signer: String::new(),
            date: String::new(),
            algorithm: Algorithm::LegacyHmacSha1,
            scope: String::new(),
            message: "what do ya want for nothing?".into(),
            portless: None,
            signature: unhex("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79").unwrap(),
        };
        assert!(verify(Key::Hmac("Jefe".into()), &prepared));
        prepared.algorithm = Algorithm::GoogleHmac;
        assert!(!verify(Key::Hmac("Jefe".into()), &prepared));
    }

    /// Public AWS documentation example credentials, never an emulator-created secret.
    const AWS_EXAMPLE_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    /// The published AWS S3 GET example, whose signature covers Host without a port, sent
    /// with the given Host.
    fn published_aws_header_request(host: &str) -> Prepared {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", host),
            ("range", "bytes=0-9"),
            ("x-amz-date", "20130524T000000Z"),
            (
                "x-amz-content-sha256",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
            ),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        v4::prepare(
            &Method::GET,
            &"/test.txt".parse().unwrap(),
            &headers,
            parse_timestamp("20130524T000000Z").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn published_aws_s3_header_vector_verifies_without_a_local_signer() {
        let prepared = published_aws_header_request("examplebucket.s3.amazonaws.com");
        assert!(verify(Key::Hmac(AWS_EXAMPLE_SECRET.into()), &prepared));
        assert!(!verify(
            Key::Hmac("incorrect public fixture".into()),
            &prepared
        ));
    }

    #[test]
    fn lenient_host_port_retries_an_hmac_signature_only_without_the_request_port() {
        use SignedHostPort::{Exact, Lenient};
        let secret = Key::Hmac(AWS_EXAMPLE_SECRET.into());
        let prepare = published_aws_header_request;
        let check = |host_port, prepared: &Prepared| {
            verify_host_forms(host_port, prepared, |message| {
                super::verify(&secret, prepared, message)
            })
        };
        let unported = prepare("examplebucket.s3.amazonaws.com");
        assert!(unported.portless.is_none());
        for host_port in [Exact, Lenient] {
            assert_eq!(check(host_port, &unported), Ok(()));
        }
        let ported = prepare("examplebucket.s3.amazonaws.com:4443");
        assert_eq!(check(Exact, &ported), Err(Failure::MismatchWithHostPort));
        assert_eq!(check(Lenient, &ported), Ok(()));
        // Matching neither form, these get no port hint and no lenient acceptance.
        let other = prepare("otherbucket.s3.amazonaws.com:4443");
        let mut tampered = prepare("examplebucket.s3.amazonaws.com:4443");
        tampered.signature[0] ^= 1;
        for host_port in [Exact, Lenient] {
            assert_eq!(check(host_port, &other), Err(Failure::Mismatch));
            assert_eq!(check(host_port, &tampered), Err(Failure::Mismatch));
        }
    }

    #[tokio::test]
    async fn closed_catalog_retains_a_backend_cause_and_returns_xml_503() {
        use std::error::Error;
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.close().await.unwrap();
        let auth = Auth::strict(
            crate::auth::Registry::parse(
                br#"{"identities":[{"id":"signer@example.invalid","tokens":["token"]}]}"#,
            )
            .unwrap(),
        );
        let clock = crate::clock::SystemClock;
        let date = timestamp(clock.now()).unwrap();
        let uri: Uri = format!("/bucket/object?X-Goog-Algorithm=GOOG4-HMAC-SHA256&X-Goog-Credential=key%2F{}%2Fauto%2Fstorage%2Fgoog4_request&X-Goog-Date={date}&X-Goog-Expires=60&X-Goog-SignedHeaders=host&X-Goog-Signature=00",&date[..8]).parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost".parse().unwrap());
        let error = auth
            .authenticate_xml_resource(
                &catalog,
                &Method::GET,
                &uri,
                &headers,
                &clock,
                &Resource::PathStyle,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, VerificationError::Catalog(_)));
        assert!(error.source().is_some());
        let error = crate::xml::authenticate_target(crate::xml::Authentication {
            auth: &auth,
            catalog: &catalog,
            method: &Method::GET,
            uri: &uri,
            headers: &headers,
            clock: &clock,
            resource: &Resource::PathStyle,
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.response(crate::xml::ErrorBody::Include).status(),
            hyper::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
