//! Runtime verification owns only one admitted crypto job, never a catalog transaction.

use super::*;
use crate::{
    bucket::CatalogError,
    storage::{Catalog, failure::BackendCause},
};
use tokio::sync::Semaphore;

static VERIFIERS: Semaphore = Semaphore::const_new(1);

pub(super) async fn crypto_job<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, VerificationError> {
    let permit = VERIFIERS
        .acquire()
        .await
        .map_err(|error| VerificationError::Worker(BackendCause::from_error(error)))?;
    crate::task::spawn_blocking("xml-signature", move || {
        let _permit = permit;
        job()
    })
    .await
    .map_err(|error| VerificationError::Worker(BackendCause::from_error(error)))?
    .map_err(|error| VerificationError::Worker(BackendCause::from_error(error)))
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
    pub algorithm: Algorithm,
    pub scope: String,
    pub message: String,
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
    Rsa(Vec<PublicKey>),
    Hmac(String),
}

pub(crate) fn has_header(headers: &HeaderMap) -> bool {
    v4::has_header(headers) || v2::has_header(headers)
}

impl Auth {
    #[cfg(test)]
    pub(crate) async fn authenticate_xml<'a>(
        &'a self,
        catalog: &Catalog,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        clock: &dyn Clock,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        self.authenticate_xml_resource(catalog, method, uri, headers, clock, &Resource::PathStyle)
            .await
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
        if headers
            .get("x-amz-content-sha256")
            .is_some_and(|value| value == STREAMING_PAYLOAD)
        {
            // Only the ordinary PUT adapter carries the authenticated chunk-verifier state.
            return Err(Failure::Malformed.into());
        }
        let Some(registry) = &self.registry else {
            // Preserve the explicitly permissive profile; no strict failure reaches this branch.
            if v2::is_v2(uri.query()) {
                self.authenticate_v2(method, uri, headers, clock)?;
            }
            return Ok(None);
        };
        v4::bounded(uri, headers)?;
        let prepared = if v2::is_v2(uri.query()) || v2::has_header(headers) {
            let mut prepared = v2::prepare_resource(method, uri, headers, clock.now(), resource)?;
            prepared.algorithm = if !v2::has_header(headers)
                && registry.signing_keys.contains_key(&prepared.signer)
            {
                Algorithm::Rsa
            } else {
                Algorithm::LegacyHmacSha1
            };
            prepared
        } else {
            v4::prepare(method, uri, headers, clock.now())?
        };
        self.verify_prepared(catalog, prepared).await.map(Some)
    }

    pub(super) async fn verify_prepared<'a>(
        &'a self,
        catalog: &Catalog,
        prepared: v4::Prepared,
    ) -> Result<Caller<'a>, VerificationError> {
        let (caller, key) = self.verification_key(catalog, &prepared).await?;
        if !crypto_job(move || verify(key, &prepared)).await? {
            return Err(Failure::Mismatch.into());
        }
        Ok(caller)
    }

    pub(super) async fn verification_key<'a>(
        &'a self,
        catalog: &Catalog,
        prepared: &Prepared,
    ) -> Result<(Caller<'a>, Key), VerificationError> {
        let registry = self.registry.as_ref().ok_or(Failure::UnknownSigner)?;
        let (signer, key) = if prepared.algorithm == v4::Algorithm::Rsa {
            (
                prepared.signer.clone(),
                Key::Rsa(
                    registry
                        .signing_keys
                        .get(&prepared.signer)
                        .ok_or(Failure::UnknownSigner)?
                        .clone(),
                ),
            )
        } else {
            let (signer, secret) = catalog
                .active_hmac_key(&prepared.signer)
                .await
                .map_err(VerificationError::Catalog)?
                .ok_or(Failure::UnknownSigner)?;
            (signer, Key::Hmac(secret))
        };
        let identity = registry
            .identities
            .iter()
            .find(|identity| identity.id == signer)
            .ok_or(Failure::UnknownSigner)?;
        Ok((Caller::Principal(identity), key))
    }
}

fn verify(key: Key, prepared: &v4::Prepared) -> bool {
    match key {
        Key::Rsa(keys) => keys
            .iter()
            .any(|key| key.verify(prepared.message.as_bytes(), &prepared.signature)),
        Key::Hmac(secret) => {
            // The legacy V2 formats use SHA-1 with the literal, underived UTF-8 secret.
            if prepared.algorithm == Algorithm::LegacyHmacSha1 {
                let key = ring::hmac::Key::new(
                    ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
                    secret.as_bytes(),
                );
                return ring::hmac::verify(&key, prepared.message.as_bytes(), &prepared.signature)
                    .is_ok();
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
            ring::hmac::verify(&key, prepared.message.as_bytes(), &prepared.signature).is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let xml = crate::xml::authenticate_target(
                &auth,
                &catalog,
                &Method::GET,
                &uri,
                &headers,
                &clock,
                &Resource::PathStyle,
            )
            .await
            .unwrap_err();
            assert_eq!(xml.status, hyper::StatusCode::SERVICE_UNAVAILABLE);
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
            algorithm: Algorithm::LegacyHmacSha1,
            scope: String::new(),
            message: "what do ya want for nothing?".into(),
            signature: unhex("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79").unwrap(),
        };
        assert!(verify(Key::Hmac("Jefe".into()), &prepared));
        prepared.algorithm = Algorithm::GoogleHmac;
        assert!(!verify(Key::Hmac("Jefe".into()), &prepared));
    }

    #[test]
    fn published_aws_s3_header_vector_verifies_without_a_local_signer() {
        // Public AWS documentation example credentials, never an emulator-created secret.
        let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "examplebucket.s3.amazonaws.com"),
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
        let prepared = v4::prepare(
            &Method::GET,
            &"/test.txt".parse().unwrap(),
            &headers,
            parse_timestamp("20130524T000000Z").unwrap(),
        )
        .unwrap();
        assert!(verify(Key::Hmac(secret.into()), &prepared));
        assert!(!verify(
            Key::Hmac("incorrect public fixture".into()),
            &prepared
        ));
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
            .authenticate_xml(&catalog, &Method::GET, &uri, &headers, &clock)
            .await
            .unwrap_err();
        assert!(matches!(error, VerificationError::Catalog(_)));
        assert!(error.source().is_some());
        let error = crate::xml::authenticate(&auth, &catalog, &Method::GET, &uri, &headers, &clock)
            .await
            .unwrap_err();
        assert_eq!(
            error.response(crate::xml::ErrorBody::Include).status(),
            hyper::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
