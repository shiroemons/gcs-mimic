//! The active authentication profile and how it resolves a request's caller.

use super::*;
use hyper::{HeaderMap, header::AUTHORIZATION};
use std::{
    borrow::Cow,
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

/// The documented 401 body cannot explain JWT setup failures, so log guidance once.
static WARNED_SELF_SIGNED_JWT: AtomicBool = AtomicBool::new(false);
/// A disabled test signer has its own advice, which an earlier general warning must not hide.
static WARNED_DISABLED_TEST_SIGNER_JWT: AtomicBool = AtomicBool::new(false);

/// Query parameters that carry credentials in Google APIs (OAuth tokens and V2/V4 signatures).
/// Unlike XML signing parameters this set includes OAuth tokens and excludes standalone Expires.
/// Strict JSON/bearer authentication rejects them instead of falling back to anonymous access;
/// the XML adapter dispatches supported V4 query credentials to the RSA verifier separately.
const CREDENTIAL_QUERY_PARAMETERS: &[&str] = &[
    "access_token",
    "oauth_token",
    "X-Goog-Signature",
    "X-Goog-Credential",
    "X-Goog-Algorithm",
    "X-Goog-Date",
    "X-Goog-Expires",
    "X-Goog-SignedHeaders",
    "Signature",
    "GoogleAccessId",
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-Expires",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
];

enum Presented<'a> {
    Anonymous,
    Token(&'a Identity),
    Jwt {
        candidate: jwt::Candidate<'a>,
        warn: bool,
    },
}

/// The active profile and how it treats signed XML requests.
#[derive(Debug, Default)]
pub(super) enum Profile {
    /// No permission checks, and XML signing credentials are ignored.
    #[default]
    Permissive,
    /// No permission checks, but a signed XML request must verify first (`signed_urls=verify`).
    /// A verified signature grants nothing an unsigned request lacks.
    VerifyingPermissive(SigningKeys),
    /// Registered principals and their grants authorize every request.
    Strict(Box<Registry>),
}

/// The active authentication profile. The default is permissive.
#[derive(Debug)]
pub struct Auth {
    pub(super) profile: Profile,
    pub(super) verifiers: signature::Verifiers,
    /// Whose V4 signatures may also verify over a canonical host without the request's port.
    pub(super) host_port: signature::HostPortPolicy,
    /// Whether the built-in test signer exists, as loading settled it.
    test_signer: TestSigner,
    /// Permissive mode only: the ACL entity (`user-EMAIL`) recorded as creator of new objects.
    /// It never authorizes anything.
    local_creator_entity: Option<crate::acl::Entity>,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            verifiers: signature::Verifiers::default(),
            host_port: signature::HostPortPolicy::default(),
            test_signer: TestSigner::Disabled,
            local_creator_entity: None,
        }
    }
}

impl Auth {
    pub fn permissive() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn strict(registry: Registry) -> Self {
        Self::with_profile(Profile::Strict(Box::new(registry)))
    }

    fn with_profile(profile: Profile) -> Self {
        Self {
            profile,
            ..Self::default()
        }
    }

    /// Loads the profile that configuration validation selected from its identity document's
    /// bytes, adding the built-in test signer when the profile's choice settles to enabled.
    /// Validation owns the pairing rules and selects that choice; only the document's contents
    /// can fail here, and only whether it declares the signer can settle the choice.
    ///
    /// `implicit_projects` are the projects where a built-in identity that the document does not
    /// declare holds its implicit role.
    pub(crate) fn load(
        profile: &AuthProfile<impl AsRef<[u8]>>,
        implicit_projects: &BTreeSet<String>,
    ) -> Result<Self, RegistryError> {
        let document = profile.identities().map(AsRef::as_ref);
        let (profile, test_signer, verification) = match profile {
            AuthProfile::Permissive => return Ok(Self::permissive()),
            AuthProfile::VerifyingPermissive { verification, .. } => {
                let (keys, signer) =
                    Registry::load_signing_keys(document, verification.test_signer)?;
                (Profile::VerifyingPermissive(keys), signer, verification)
            }
            AuthProfile::Strict {
                verification,
                trusted,
                ..
            } => {
                let (registry, signer) = Registry::load_principals(
                    document,
                    verification.test_signer,
                    trusted,
                    implicit_projects,
                )?;
                (Profile::Strict(Box::new(registry)), signer, verification)
            }
        };
        Ok(Self {
            host_port: signature::HostPortPolicy::resolve(
                verification.signed_host_port,
                test_signer,
            ),
            test_signer,
            ..Self::with_profile(profile)
        })
    }

    /// Whether the built-in test signer exists, as loading settled it.
    pub(crate) fn test_signer(&self) -> TestSigner {
        self.test_signer
    }

    /// The identity that requests without credentials on the trusted listener act as, named by
    /// `trusted_principal` or built in.
    pub(crate) fn trusted_principal(&self) -> Option<&str> {
        let registry = self.registry()?;
        registry.identities.get(registry.trusted?).map(Identity::id)
    }

    /// Whether the identity document declares `id`, as opposed to gcs-mimic adding it.
    pub(crate) fn declares(&self, id: &str) -> bool {
        self.registry()
            .is_some_and(|registry| registry.declares(id))
    }

    /// Whose V4 signatures also verify over a canonical host without the request's port, after
    /// the default that the settled test signer selects.
    pub(crate) fn host_port_policy(&self) -> signature::HostPortPolicy {
        self.host_port
    }

    /// Permissive verification with an identity file's keys and the built-in test signer.
    #[cfg(test)]
    pub(crate) fn verifying_permissive(identity_file: &[u8]) -> Self {
        let mut keys = Registry::parse_for(identity_file, IdentityUse::SigningKeys)
            .unwrap()
            .signing_keys;
        keys.add(test_signer::ID, test_signer::public_key());
        Self {
            test_signer: TestSigner::Enabled,
            ..Self::with_profile(Profile::VerifyingPermissive(keys))
        }
    }

    /// The projects strict mode's identity file names; none without one.
    pub(crate) fn project_ids(&self) -> impl Iterator<Item = &str> {
        self.registry().into_iter().flat_map(Registry::project_ids)
    }

    fn registry(&self) -> Option<&Registry> {
        match &self.profile {
            Profile::Strict(registry) => Some(registry),
            Profile::Permissive | Profile::VerifyingPermissive(_) => None,
        }
    }

    /// Whether signed XML requests verify before they proceed.
    pub(crate) fn signed_urls(&self) -> SignedUrls {
        match self.profile {
            Profile::Permissive => SignedUrls::Ignore,
            Profile::VerifyingPermissive(_) | Profile::Strict(_) => SignedUrls::Verify,
        }
    }

    /// Applies an explicit `signed_host_port` to every signer, as [`Auth::load`] does for the
    /// configured value.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_signed_host_port(
        mut self,
        signed_host_port: signature::SignedHostPort,
    ) -> Self {
        self.host_port =
            signature::HostPortPolicy::resolve(Some(signed_host_port), self.test_signer);
        self
    }

    /// Reserve half the runtime's blocking slots for cryptography; other work shares the pool.
    pub(crate) fn with_verifier_budget(mut self, max_blocking_threads: usize) -> Self {
        self.verifiers = signature::Verifiers::for_blocking_threads(max_blocking_threads);
        self
    }

    /// Records `email` as the creator of objects written in permissive mode. Strict mode ignores
    /// it because its callers are registered principals. `email` passed
    /// `validate_local_creator_email` when the setting loaded; an address that failed it only
    /// reaches here while startup is failing on that diagnostic, and records no creator.
    pub(crate) fn with_local_creator_email(mut self, email: &str) -> Self {
        self.local_creator_entity = acl_identity::creator_entity(email).ok();
        self
    }

    pub(crate) fn local_creator_entity(&self) -> Option<&crate::acl::Entity> {
        self.local_creator_entity.as_ref()
    }

    /// Points the trusted listener at `caller`, as loading a strict profile does.
    #[cfg(test)]
    pub(crate) fn trust(&mut self, caller: &TrustedCaller) {
        let Profile::Strict(registry) = &mut self.profile else {
            panic!("only strict mode has a trusted listener");
        };
        registry.trust(caller, &BTreeSet::new()).unwrap();
    }

    pub(crate) fn mode(&self) -> AuthMode {
        match self.profile {
            Profile::Permissive | Profile::VerifyingPermissive(_) => AuthMode::Permissive,
            Profile::Strict(_) => AuthMode::Strict,
        }
    }

    /// The identities the identity file declares; the built-in test signer is not counted.
    pub(crate) fn identity_count(&self) -> Option<usize> {
        self.registry().map(|registry| registry.declared)
    }

    pub(crate) fn principals(&self) -> Option<&[Identity]> {
        self.registry()
            .map(|registry| registry.identities.as_slice())
    }

    /// Local simulation of the effective parent organization policy, loaded before readiness.
    pub(crate) fn public_access_prevention_projects(&self) -> &[String] {
        self.registry()
            .map_or(&[], |registry| &registry.public_access_prevention_projects)
    }

    /// Resolve the caller from request metadata only; never reads a body or the catalog.
    ///
    /// Returns `Ok(None)` in permissive mode, where no permission checks apply.
    #[cfg(test)]
    pub(crate) async fn authenticate(
        &self,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<Option<Caller<'_>>, AuthFailure> {
        self.authenticate_on(ListenerRole::Public, headers, query)
            .await
    }

    /// [`Self::authenticate`] for a request that arrived on a `listener`.
    #[cfg(test)]
    pub(crate) async fn authenticate_on(
        &self,
        listener: ListenerRole,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<Option<Caller<'_>>, AuthFailure> {
        match self
            .authenticate_request(headers, query, &crate::clock::SystemClock, listener)
            .await
        {
            Ok(caller) => Ok(caller),
            Err(AuthError::Failure(failure)) => Err(failure),
            Err(AuthError::Unavailable(error)) => panic!("credential verifier failed: {error}"),
        }
    }

    /// The one place a listener's role affects authentication: only a request that presents no
    /// credentials at all changes, so invalid credentials fail on every listener.
    pub(crate) async fn authenticate_request(
        &self,
        headers: &HeaderMap,
        query: Option<&str>,
        clock: &dyn crate::clock::Clock,
        listener: ListenerRole,
    ) -> Result<Option<Caller<'_>>, AuthError> {
        let Some(registry) = self.registry() else {
            return Ok(None);
        };
        let identity = match Self::presented(registry, headers, query, clock)? {
            Presented::Anonymous => return Ok(Some(registry.caller_without_credentials(listener))),
            Presented::Token(identity) => Some(identity),
            Presented::Jwt { candidate, warn } => {
                let identity = candidate.verify(&self.verifiers).await?;
                if identity.is_none() && warn {
                    warn_self_signed_jwt(jwt::Rejection::Unsupported);
                }
                identity
            }
        };
        identity
            .map(|identity| Some(Caller::Principal(identity)))
            .ok_or_else(|| AuthFailure::InvalidCredentials.into())
    }

    fn presented<'a>(
        registry: &'a Registry,
        headers: &HeaderMap,
        query: Option<&str>,
        clock: &dyn crate::clock::Clock,
    ) -> Result<Presented<'a>, AuthFailure> {
        if query.is_some_and(has_credential_parameter) {
            return Err(AuthFailure::InvalidCredentials);
        }
        use crate::http_header::{TextOrder, sole};
        let Some(value) = sole(headers, AUTHORIZATION)
            .text(TextOrder::RepetitionFirst)
            .map_err(|_| AuthFailure::InvalidCredentials)?
        else {
            return Ok(Presented::Anonymous);
        };
        let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(AuthFailure::InvalidCredentials);
        }
        let token = token.trim_start_matches(' ');
        // Registry tokens are visible ASCII, so an empty or space-containing token never matches.
        if jwt_shaped(token) {
            // A registered token must never bypass verification of JWT-shaped credentials.
            let warn = looks_like_jwt(token);
            jwt::Candidate::parse(registry, token, clock)
                .map(|candidate| Presented::Jwt { candidate, warn })
                .map_err(|rejection| {
                    if warn {
                        warn_self_signed_jwt(rejection);
                    }
                    AuthFailure::InvalidCredentials
                })
        } else {
            // Randomized SipHash indexes local test tokens; HMAC verification uses ring's constant-time check.
            registry
                .tokens
                .get(token)
                .and_then(|index| registry.identities.get(*index))
                .map(Presented::Token)
                .ok_or(AuthFailure::InvalidCredentials)
        }
    }
}

/// Logs the advice for `rejection` once per cause, so an earlier unrelated rejection cannot hide
/// the disabled test signer. The message never includes the token.
fn warn_self_signed_jwt(rejection: jwt::Rejection) {
    let (warned, message): (_, Cow<'static, str>) = match rejection {
        jwt::Rejection::Unsupported => (
            &WARNED_SELF_SIGNED_JWT,
            "strict authentication rejected a self-signed JWT; register its issuer and RSA public_keys in the identity file, use RS256 with matching issuer/subject, devstorage.full_control scope, no audience and a valid lifetime of at most one hour; the permissive profile remains available for unauthenticated local clients".into(),
        ),
        jwt::Rejection::DisabledTestSigner => (
            &WARNED_DISABLED_TEST_SIGNER_JWT,
            format!(
                "strict authentication rejected a self-signed JWT from the built-in test signer {} because the test signer is disabled; set GCS_MIMIC_TEST_SIGNER=enabled (--test-signer enabled) to accept the key that gcs-mimic test-signer credentials prints",
                test_signer::ID
            )
            .into(),
        ),
    };
    if !warned.swap(true, Ordering::Relaxed) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            crate::diagnostics::Event::AuthSelfSignedJwtRejected,
            serde_json::json!({ "message": message }),
        );
    }
}

pub(super) fn jwt_shaped(token: &str) -> bool {
    token.split('.').count() == 3
}

/// A JWT is three base64url segments; its header is JSON, which encodes to a leading `eyJ`.
pub(super) fn looks_like_jwt(token: &str) -> bool {
    let mut segments = token.split('.');
    segments
        .next()
        .is_some_and(|header| header.starts_with("eyJ"))
        && segments.next().is_some_and(|payload| !payload.is_empty())
        && segments
            .next()
            .is_some_and(|signature| !signature.is_empty())
        && segments.next().is_none()
}

fn has_credential_parameter(query: &str) -> bool {
    query
        .split('&')
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .any(|key| {
            let key = lenient_decode(key);
            CREDENTIAL_QUERY_PARAMETERS
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name.as_bytes()))
        })
}

/// Percent-decode a query key without failing, so malformed encodings cannot hide credentials.
pub(super) fn lenient_decode(raw: &str) -> Vec<u8> {
    let mut bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    while let Some((&first, tail)) = bytes.split_first() {
        if first == b'%'
            && let Some(byte) = tail.first_chunk::<2>().copied().and_then(crate::hex::byte)
        {
            decoded.push(byte);
            bytes = tail.get(2..).unwrap_or_default();
            continue;
        }
        decoded.push(if first == b'+' { b' ' } else { first });
        bytes = tail;
    }
    decoded
}
