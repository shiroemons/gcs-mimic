//! Scoped local service-account JWTs; never fetch keys or exchange OAuth assertions.

use super::{
    AuthError, Identity, Registry,
    signature::{PublicKey, Verifiers},
};
use crate::clock::Clock;
use serde::Deserialize;
use std::{sync::Arc, time::UNIX_EPOCH};

const MAX_TOKEN_BYTES: usize = 8192;
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

impl<'a> Candidate<'a> {
    pub(super) fn parse(registry: &'a Registry, token: &str, clock: &dyn Clock) -> Option<Self> {
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
        let keys = Arc::clone(registry.signing_keys.get(&claims.iss)?);
        let signature = decode(signature)?;
        let identity = registry
            .identities
            .iter()
            .find(|identity| identity.id == claims.iss)?;
        Some(Self {
            identity,
            keys,
            message: message.into(),
            signature,
        })
    }

    pub(super) fn verify_sync(self) -> Option<&'a Identity> {
        self.keys
            .iter()
            .any(|key| key.verify(self.message.as_bytes(), &self.signature))
            .then_some(self.identity)
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

#[cfg(test)]
fn authenticate<'a>(
    registry: &'a Registry,
    token: &str,
    clock: &dyn Clock,
) -> Option<&'a Identity> {
    Candidate::parse(registry, token, clock)?.verify_sync()
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
