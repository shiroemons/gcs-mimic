//! RSA verification uses an Auth-owned bounded blocking budget, never a catalog transaction.
//! Bounded HMAC chains for streaming seeds and individual chunks run inline.

use super::*;
use crate::{
    auth::{Profile, Registry, SigningKeys, test_signer},
    bucket::CatalogError,
    cause::BackendCause,
};
use ring::hmac;
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

/// How a prepared signature is checked once its signer's key is known.
#[cfg_attr(test, derive(Debug))]
pub(super) enum Scheme {
    /// RSASSA-PKCS1-v1_5 SHA-256 against the signer's public keys: V4 `GOOG4-RSA-SHA256`, and V2
    /// query or policy signatures by a signer with a registered key.
    Rsa,
    /// HMAC-SHA256 under a key derived from the secret and the V4 credential scope.
    HmacV4(V4Scope),
    /// HMAC-SHA1 under the literal UTF-8 secret: every legacy V2 HMAC signature.
    HmacSha1,
}

/// The V4 credential scope an HMAC signing key derives from.
#[cfg_attr(test, derive(Debug))]
pub(super) struct V4Scope {
    pub(super) family: v4::Family,
    /// `X-Goog-Date` or `X-Amz-Date`; aws-chunked chunk signatures repeat it.
    pub(super) date: String,
    /// `DATE/REGION/SERVICE/REQUEST`.
    pub(super) scope: String,
}

impl V4Scope {
    /// The family prefix and the literal UTF-8 secret, then one HMAC round per scope
    /// component. The API returns a base64-looking secret, but its text is the input.
    pub(super) fn signing_key(&self, secret: &str) -> hmac::Key {
        let prefixed = format!("{}{secret}", self.family.key_prefix());
        self.scope.split('/').fold(
            hmac::Key::new(hmac::HMAC_SHA256, prefixed.as_bytes()),
            |key, component| {
                hmac::Key::new(
                    hmac::HMAC_SHA256,
                    hmac::sign(&key, component.as_bytes()).as_ref(),
                )
            },
        )
    }
}

/// The legacy V2 formats use SHA-1 with the literal, underived UTF-8 secret.
fn legacy_hmac_key(secret: &str) -> hmac::Key {
    hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret.as_bytes())
}

pub(super) struct Prepared {
    pub(crate) signer: String,
    pub(crate) scheme: Scheme,
    pub(crate) message: String,
    /// For a V4 signature over a Host that names an explicit port, the string to sign with that
    /// port removed from the canonical host. Signatures that do not cover Host have none.
    pub(crate) portless: Option<String>,
    pub(crate) signature: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum VerificationError {
    #[error("Invalid XML signature")]
    Signature(Failure),
    #[error("Signature verifier unavailable")]
    Catalog(#[source] CatalogError),
    #[error("Signature verifier unavailable")]
    Worker(#[source] BackendCause),
}

impl From<Failure> for VerificationError {
    fn from(value: Failure) -> Self {
        Self::Signature(value)
    }
}

/// The key a signature verifies against, resolved for its scheme.
pub(super) enum Key {
    Rsa(Arc<[PublicKey]>),
    /// An HMAC key whose algorithm the scheme fixed: SHA-1 for legacy V2, SHA-256 for V4.
    Hmac(hmac::Key),
}

impl Key {
    fn verify(&self, message: &str, signature: &[u8]) -> bool {
        match self {
            Self::Rsa(keys) => keys
                .iter()
                .any(|key| key.verify(message.as_bytes(), signature)),
            Self::Hmac(key) => hmac::verify(key, message.as_bytes(), signature).is_ok(),
        }
    }
}

/// A V2 signer with a registered RSA key signs with RSA; any other access ID names an HMAC
/// key. RSA failure never retries with HMAC.
pub(super) fn v2_scheme(keys: &SigningKeys, signer: &str) -> Scheme {
    if keys.contains(signer) {
        Scheme::Rsa
    } else {
        Scheme::HmacSha1
    }
}

/// The store of the HMAC keys that verify signatures. The catalog keeps them; verification names
/// only this lookup, so `auth` stays below `storage`.
pub(crate) trait HmacKeys {
    /// The service account and secret of the ACTIVE key that `access_id` names. Each lookup
    /// reads fresh signing material, so a revocation takes effect without a secret cache.
    fn active_hmac_key(
        &self,
        access_id: &str,
    ) -> impl Future<Output = Result<Option<(String, String)>, CatalogError>> + Send;
}

/// The ACTIVE HMAC secret an access ID names, and whom it makes the caller. The ACTIVE key
/// alone is enough when a verified signature authorizes nothing by identity.
pub(super) async fn hmac_secret<'a>(
    keys: &impl HmacKeys,
    signers: &Signers<'a>,
    access_id: &str,
) -> Result<(Option<Caller<'a>>, String), VerificationError> {
    let (signer, secret) = keys
        .active_hmac_key(access_id)
        .await
        .map_err(VerificationError::Catalog)?
        .ok_or_else(|| signers.unknown(access_id))?;
    Ok((signers.caller(&signer)?, secret))
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
        keys: &impl HmacKeys,
        request: SignedRequest<'_>,
        clock: &dyn Clock,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        let SignedRequest {
            kind,
            method,
            uri,
            headers,
            resource,
        } = request;
        if is_streaming_payload(headers) {
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
        let now = clock.now();
        let prepared = match kind {
            SignatureKind::V2Query => {
                v2::prepare_query(method, uri, headers, now, resource, signers.keys())?
            }
            SignatureKind::LegacyHeader => {
                v2::legacy::prepare(method, uri, headers, now, resource)?
            }
            SignatureKind::V4 => v4::prepare(method, uri, headers, now)?,
        };
        self.verify_prepared(keys, prepared).await
    }

    /// The caller is `None` when a verified signature leaves access permissive.
    pub(super) async fn verify_prepared<'a>(
        &'a self,
        keys: &impl HmacKeys,
        prepared: v4::Prepared,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        let (caller, key) = self.verification_key(keys, &prepared).await?;
        let host_port = self.host_port.for_signature(&prepared);
        self.verifiers
            .run(move || {
                verify_host_forms(host_port, &prepared, |message| {
                    key.verify(message, &prepared.signature)
                })
            })
            .await??;
        Ok(caller)
    }

    /// RSA keys come from the active signers; HMAC keys from the key store.
    async fn verification_key<'a>(
        &'a self,
        keys: &impl HmacKeys,
        prepared: &Prepared,
    ) -> Result<(Option<Caller<'a>>, Key), VerificationError> {
        // Every caller has checked that signatures are verified; this keeps the same answer.
        let signers = self
            .signers()
            .ok_or(Failure::UnknownSigner(KeyLookup::Ignored))?;
        match &prepared.scheme {
            Scheme::Rsa => {
                let keys = signers
                    .keys()
                    .get(&prepared.signer)
                    .ok_or_else(|| signers.unknown(&prepared.signer))?;
                Ok((
                    signers.caller(&prepared.signer)?,
                    Key::Rsa(Arc::clone(keys)),
                ))
            }
            Scheme::HmacSha1 => {
                let (caller, secret) = hmac_secret(keys, &signers, &prepared.signer).await?;
                Ok((caller, Key::Hmac(legacy_hmac_key(&secret))))
            }
            Scheme::HmacV4(scope) => {
                let (caller, secret) = hmac_secret(keys, &signers, &prepared.signer).await?;
                Ok((caller, Key::Hmac(scope.signing_key(&secret))))
            }
        }
    }
}

impl HostPortPolicy {
    /// The rule for one signature. Only an RSA signature names its signer before verification,
    /// so an HMAC signature, whose access ID names a key rather than the test signer, stays
    /// exact under the test signer's default.
    pub(super) fn for_signature(self, prepared: &Prepared) -> SignedHostPort {
        match self {
            Self::Lenient => SignedHostPort::Lenient,
            Self::LenientForTestSigner
                if matches!(prepared.scheme, Scheme::Rsa) && prepared.signer == test_signer::ID =>
            {
                SignedHostPort::Lenient
            }
            Self::Exact | Self::LenientForTestSigner => SignedHostPort::Exact,
        }
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
            crate::diagnostics::Event::SignatureHostPortOmitted,
            serde_json::json!({
                "message": "a V4 signature matched only without the port of the request Host because signed_host_port=lenient; its signer omitted the port, as Go storage does. Sign the final host and port, for example with gcs-mimic url sign, to verify it exactly; this warning is logged once per process",
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Catalog;
    use std::assert_matches;

    /// Verifies the string to sign over the request as sent, which the published vectors cover.
    fn verify(key: Key, prepared: &Prepared) -> bool {
        key.verify(&prepared.message, &prepared.signature)
    }

    /// The key a V4 HMAC signature verifies against for `secret`.
    fn v4_key(prepared: &Prepared, secret: &str) -> Key {
        let Scheme::HmacV4(scope) = &prepared.scheme else {
            panic!("not a V4 HMAC signature");
        };
        Key::Hmac(scope.signing_key(secret))
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
            let request = SignedRequest {
                kind: SignatureKind::of(uri.query(), &headers).unwrap(),
                method: &Method::GET,
                uri: &uri,
                headers: &headers,
                resource: &Resource::PathStyle,
            };
            let failure = auth
                .authenticate_xml_resource(&catalog, request, &clock)
                .await
                .unwrap_err();
            assert_matches!(failure, VerificationError::Catalog(_));
            assert!(failure.source().is_some());
            let xml = crate::xml::authenticate_target(crate::xml::Authentication {
                auth: &auth,
                catalog: &catalog,
                method: &Method::GET,
                uri: &uri,
                headers: &headers,
                clock: &clock,
                resource: &Resource::PathStyle,
                listener: crate::auth::ListenerRole::Public,
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
        let prepared = v2::legacy::prepare(
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
        assert_matches!(prepared.scheme, Scheme::HmacSha1);
        assert!(verify(
            Key::Hmac(legacy_hmac_key("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")),
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
            assert_matches!(
                v2::prepare(
                    &Method::GET,
                    &uri,
                    &HeaderMap::new(),
                    UNIX_EPOCH + Duration::from_secs(seconds)
                )
                .map(drop),
                Err(Failure::Expired)
            );
        }
        // A signer without a registered RSA key names an HMAC key.
        let mut prepared = v2::prepare(
            &Method::GET,
            &uri,
            &HeaderMap::new(),
            UNIX_EPOCH + Duration::from_secs(1175139600),
        )
        .unwrap();
        assert_matches!(prepared.scheme, Scheme::HmacSha1);
        assert_eq!(
            prepared.message,
            "GET\n\n\n1175139620\n/awsexamplebucket1/photos/puppy.jpg"
        );
        let key = || Key::Hmac(legacy_hmac_key("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"));
        // The archive changed the bucket but retained the older bucket's query signature.
        assert!(!verify(key(), &prepared));
        // Independently generated with OpenSSL against the archive's literal new-bucket input.
        prepared.signature = v2::decode_signature("1No4mq5ETf02z8aet9voy6gui6E=").unwrap();
        assert!(verify(key(), &prepared));
        prepared.message.push('x');
        assert!(!verify(key(), &prepared));
    }

    #[test]
    fn legacy_policy_hmac_sha1_matches_rfc2202_without_v4_derivation() {
        let prepared = Prepared {
            signer: String::new(),
            scheme: Scheme::HmacSha1,
            message: "what do ya want for nothing?".into(),
            portless: None,
            signature: unhex("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79").unwrap(),
        };
        assert!(verify(Key::Hmac(legacy_hmac_key("Jefe")), &prepared));
        let derived = V4Scope {
            family: v4::Family::Google,
            date: String::new(),
            scope: String::new(),
        };
        assert!(!verify(Key::Hmac(derived.signing_key("Jefe")), &prepared));
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
        assert!(verify(v4_key(&prepared, AWS_EXAMPLE_SECRET), &prepared));
        assert!(!verify(
            v4_key(&prepared, "incorrect public fixture"),
            &prepared
        ));
    }

    #[test]
    fn lenient_host_port_retries_an_hmac_signature_only_without_the_request_port() {
        use SignedHostPort::{Exact, Lenient};
        let prepare = published_aws_header_request;
        let unported = prepare("examplebucket.s3.amazonaws.com");
        // Every request below signs with the same credential scope.
        let key = v4_key(&unported, AWS_EXAMPLE_SECRET);
        let check = |host_port, prepared: &Prepared| {
            verify_host_forms(host_port, prepared, |message| {
                key.verify(message, &prepared.signature)
            })
        };
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

    /// Without the setting only the built-in test signer's RSA signatures may omit the port: a
    /// registered key and every HMAC key, even one whose access ID reads like the signer's ID,
    /// stay exact. An explicit value applies to every signature.
    #[test]
    fn the_default_lenient_host_port_covers_only_the_test_signers_rsa_signatures() {
        use HostPortPolicy::{Exact, Lenient, LenientForTestSigner};
        let hmac = || published_aws_header_request("examplebucket.s3.amazonaws.com:4443");
        let rsa = |signer: &str| Prepared {
            scheme: Scheme::Rsa,
            signer: signer.to_owned(),
            ..hmac()
        };
        let named_like_the_signer = Prepared {
            signer: test_signer::ID.to_owned(),
            ..hmac()
        };
        let signatures = [
            (rsa(test_signer::ID), SignedHostPort::Lenient),
            (rsa("viewer@example.invalid"), SignedHostPort::Exact),
            (hmac(), SignedHostPort::Exact),
            (named_like_the_signer, SignedHostPort::Exact),
        ];
        for (prepared, by_default) in &signatures {
            let signer = &prepared.signer;
            assert_eq!(
                LenientForTestSigner.for_signature(prepared),
                *by_default,
                "{signer}"
            );
            assert_eq!(Lenient.for_signature(prepared), SignedHostPort::Lenient);
            assert_eq!(Exact.for_signature(prepared), SignedHostPort::Exact);
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
        let request = SignedRequest {
            kind: SignatureKind::of(uri.query(), &headers).unwrap(),
            method: &Method::GET,
            uri: &uri,
            headers: &headers,
            resource: &Resource::PathStyle,
        };
        let error = auth
            .authenticate_xml_resource(&catalog, request, &clock)
            .await
            .unwrap_err();
        assert_matches!(error, VerificationError::Catalog(_));
        assert!(error.source().is_some());
        let error = crate::xml::authenticate_target(crate::xml::Authentication {
            auth: &auth,
            catalog: &catalog,
            method: &Method::GET,
            uri: &uri,
            headers: &headers,
            clock: &clock,
            resource: &Resource::PathStyle,
            listener: crate::auth::ListenerRole::Public,
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.response(crate::xml::ErrorBody::Include).status(),
            hyper::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    use std::error::Error as _;
    let signature = VerificationError::Signature(Failure::Malformed);
    assert_eq!(signature.to_string(), "Invalid XML signature");
    assert!(signature.source().is_none());
    let catalog = VerificationError::Catalog(CatalogError::NotFound);
    assert_eq!(catalog.to_string(), "Signature verifier unavailable");
    assert!(catalog.source().unwrap().is::<CatalogError>());
    let worker = VerificationError::Worker(crate::cause::BackendFailure::CatalogClosed.into());
    assert_eq!(worker.to_string(), "Signature verifier unavailable");
    assert!(worker.source().unwrap().is::<BackendCause>());
}
