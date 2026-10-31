//! Scoped local service-account JWTs; never fetch keys or exchange OAuth assertions.

use super::{Identity, Registry};
use crate::clock::Clock;
use serde::Deserialize;
use std::time::UNIX_EPOCH;

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

pub(super) fn authenticate<'a>(
    registry: &'a Registry,
    token: &str,
    clock: &dyn Clock,
) -> Option<&'a Identity> {
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
    let keys = registry.signing_keys.get(&claims.iss)?;
    let signature = decode(signature)?;
    if !keys
        .iter()
        .any(|key| key.verify(message.as_bytes(), &signature))
    {
        return None;
    }
    registry
        .identities
        .iter()
        .find(|identity| identity.id == claims.iss)
}

fn decode(raw: &str) -> Option<Vec<u8>> {
    if raw.is_empty() || raw.len() % 4 == 1 {
        return None;
    }
    let mut output = Vec::with_capacity(raw.len() * 3 / 4);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in raw.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(digit);
        count += 6;
        if count >= 8 {
            count -= 8;
            output.push((bits >> count) as u8);
        }
    }
    (bits & ((1 << count) - 1) == 0).then_some(output)
}

#[cfg(test)]
mod tests;
