//! Scoped local service-account JWTs; never fetch keys or exchange OAuth assertions.

use super::{
    AuthError, Identity, Registry,
    signature::{PublicKey, Verifiers},
    test_signer,
};
use crate::clock::Clock;
use serde::Deserialize;
use std::{sync::Arc, time::UNIX_EPOCH};

pub(super) const MAX_TOKEN_BYTES: usize = 8192;
const SCOPE: &str = "https://www.googleapis.com/auth/devstorage.full_control";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: Option<String>,
    kid: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    sub: String,
    scope: String,
    iat: u64,
    exp: u64,
    nbf: Option<u64>,
}

pub(super) struct Candidate<'a> {
    identity: &'a Identity,
    keys: Arc<[PublicKey]>,
    message: String,
    signature: Vec<u8>,
}

/// Why a JWT-shaped bearer token was refused before its signature was checked. The response
/// says only that the credentials are invalid; the cause goes to the operator's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rejection {
    /// Outside the supported header and claims, or from an issuer without registered keys.
    Unsupported,
    /// From the built-in test signer while it is disabled. Rails and other clients sign with
    /// the key that `test-signer credentials` prints, so this names a likely setup mistake.
    DisabledTestSigner,
}

impl<'a> Candidate<'a> {
    pub(super) fn parse(
        registry: &'a Registry,
        token: &str,
        clock: &dyn Clock,
    ) -> Result<Self, Rejection> {
        let (message, issuer, signature) =
            Self::unverified(token, clock).ok_or(Rejection::Unsupported)?;
        // The enabled test signer always has its published key, so a missing one means disabled.
        let keys = registry
            .signing_keys
            .get(&issuer)
            .ok_or(if issuer == test_signer::ID {
                Rejection::DisabledTestSigner
            } else {
                Rejection::Unsupported
            })?;
        let identity = registry.identity(&issuer).ok_or(Rejection::Unsupported)?;
        Ok(Self {
            identity,
            keys: Arc::clone(keys),
            message: message.into(),
            signature,
        })
    }

    /// The signed message, the issuer and the signature of a token whose header and claims fit
    /// the supported subset, before any key is looked up.
    fn unverified<'t>(token: &'t str, clock: &dyn Clock) -> Option<(&'t str, String, Vec<u8>)> {
        if token.len() > MAX_TOKEN_BYTES {
            return None;
        }
        let (message, signature) = token.rsplit_once('.')?;
        let (header, claims) = message.split_once('.')?;
        let header: Header = serde_json::from_slice(&decode(header)?).ok()?;
        if header.alg != "RS256"
            || header.typ.as_deref().is_some_and(|typ| typ != "JWT")
            || header
                .kid
                .as_ref()
                .is_some_and(|kid| kid.is_empty() || kid.len() > 256)
        {
            return None;
        }
        // This subset deliberately rejects aud, including OAuth token-exchange assertions,
        // and narrower scopes whose permissions would require a separate authorization cap.
        let claims: Claims = serde_json::from_slice(&decode(claims)?).ok()?;
        let now = clock.now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        if claims.iss != claims.sub
            || claims.scope != SCOPE
            || claims.iat > now
            || claims.exp <= now
            || claims.exp.checked_sub(claims.iat)? > 3600
            || claims.nbf.is_some_and(|nbf| nbf > now || nbf >= claims.exp)
        {
            return None;
        }
        Some((message, claims.iss, decode(signature)?))
    }

    pub(super) async fn verify(
        self,
        verifiers: &Verifiers,
    ) -> Result<Option<&'a Identity>, AuthError> {
        let Self {
            identity,
            keys,
            message,
            signature,
        } = self;
        let valid = verifiers
            .run(move || {
                keys.iter()
                    .any(|key| key.verify(message.as_bytes(), &signature))
            })
            .await
            .map_err(|error| {
                AuthError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
            })?;
        Ok(valid.then_some(identity))
    }
}

fn decode(raw: &str) -> Option<Vec<u8>> {
    if raw.is_empty() {
        return None;
    }
    crate::base64::decode(
        raw,
        crate::base64::Alphabet::UrlSafe,
        crate::base64::Padding::Forbidden,
    )
}

#[cfg(test)]
mod tests;
