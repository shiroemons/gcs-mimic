//! Bounded, local V4 RSA query signing and verification. No Google IAM calls.

use super::{Auth, Caller};
use crate::{clock::Clock, json};
use hyper::{HeaderMap, Method, Uri};
use ring::{digest, rand::SystemRandom, signature};
use std::{
    collections::BTreeMap,
    fmt::Write,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, SubjectPublicKeyInfoDer, pem::PemObject};

pub const PARAMETERS: &[&str] = &[
    "X-Goog-Algorithm",
    "X-Goog-Credential",
    "X-Goog-Date",
    "X-Goog-Expires",
    "X-Goog-SignedHeaders",
    "X-Goog-Signature",
];
const ALGORITHM: &str = "GOOG4-RSA-SHA256";
const MAX_EXPIRES: u64 = 604800;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Malformed,
    UnknownSigner,
    Mismatch,
    Expired,
    NotYetValid,
}

#[derive(Debug)]
pub(super) struct PublicKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

impl PublicKey {
    pub(super) fn parse(pem: &str) -> Result<Self, String> {
        let invalid = || {
            "public_keys must contain only RSA SubjectPublicKeyInfo PEM (2048..8192 bits)"
                .to_owned()
        };
        if pem.len() > 4096
            || !pem.trim().starts_with("-----BEGIN PUBLIC KEY-----")
            || !pem.trim().ends_with("-----END PUBLIC KEY-----")
            || pem.matches("-----BEGIN").count() != 1
        {
            return Err(invalid());
        }
        let der = SubjectPublicKeyInfoDer::from_pem_slice(pem.as_bytes()).map_err(|_| invalid())?;
        let mut outer = der.as_ref();
        let mut spki = element(&mut outer, 0x30).ok_or_else(invalid)?;
        if !outer.is_empty()
            || element(&mut spki, 0x30)
                != Some(
                    &[
                        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05,
                        0x00,
                    ][..],
                )
        {
            return Err(invalid());
        }
        let bits = element(&mut spki, 0x03).ok_or_else(invalid)?;
        if !spki.is_empty() || bits.first() != Some(&0) {
            return Err(invalid());
        }
        let mut bits = &bits[1..];
        let mut rsa = element(&mut bits, 0x30).ok_or_else(invalid)?;
        let n = integer(&mut rsa).ok_or_else(invalid)?;
        let e = integer(&mut rsa).ok_or_else(invalid)?;
        let exponent = e
            .iter()
            .try_fold(0u64, |v, b| v.checked_mul(256)?.checked_add(u64::from(*b)))
            .ok_or_else(invalid)?;
        let bit_len = n.len() * 8 - n.first().ok_or_else(invalid)?.leading_zeros() as usize;
        if !bits.is_empty()
            || !rsa.is_empty()
            || !(2048..=8192).contains(&bit_len)
            || n.last().is_none_or(|b| b & 1 == 0)
            || !(3..=(1u64 << 33) - 1).contains(&exponent)
            || exponent & 1 == 0
        {
            return Err(invalid());
        }
        Ok(Self {
            n: n.to_vec(),
            e: e.to_vec(),
        })
    }

    fn verify(&self, message: &[u8], bytes: &[u8]) -> bool {
        signature::RsaPublicKeyComponents {
            n: &self.n,
            e: &self.e,
        }
        .verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, bytes)
        .is_ok()
    }
}

// Only the fixed RSA SPKI grammar is needed; reject non-minimal DER and trailing fields.
fn element<'a>(input: &mut &'a [u8], tag: u8) -> Option<&'a [u8]> {
    if *input.first()? != tag {
        return None;
    }
    let first = *input.get(1)?;
    let (len, start) = if first < 128 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 127);
        if !(1..=2).contains(&count) || *input.get(2)? == 0 {
            return None;
        }
        let len = input
            .get(2..2 + count)?
            .iter()
            .fold(0usize, |n, b| n * 256 + usize::from(*b));
        if len < 128 {
            return None;
        }
        (len, 2 + count)
    };
    let value = input.get(start..start + len)?;
    *input = &input[start + len..];
    Some(value)
}

fn integer<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let value = element(input, 0x02)?;
    match value {
        [0, next, ..] if next & 128 != 0 => Some(&value[1..]),
        [first, ..] if *first > 0 && first & 128 == 0 => Some(value),
        _ => None,
    }
}

pub fn is_signed(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        query.split('&').any(|pair| {
            let key = super::lenient_decode(pair.split('=').next().unwrap_or_default());
            PARAMETERS
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name.as_bytes()))
        })
    })
}

impl Auth {
    /// The permissive profile ignores credentials; strict XML requests never fall back after failure.
    pub fn authenticate_signed(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        clock: &dyn Clock,
    ) -> Result<Option<Caller<'_>>, Failure> {
        let Some(registry) = &self.registry else {
            return Ok(None);
        };
        if headers.contains_key("authorization") {
            return Err(Failure::Malformed);
        }
        let query =
            json::parse_query(uri.query().unwrap_or_default()).map_err(|_| Failure::Malformed)?;
        let required = |key: &str| {
            query
                .get(key)
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .ok_or(Failure::Malformed)
        };
        if required("X-Goog-Algorithm")? != ALGORITHM {
            return Err(Failure::Malformed);
        }
        let credential = required("X-Goog-Credential")?;
        let parts: Vec<_> = credential.split('/').collect();
        if parts.len() != 5
            || parts[0].is_empty()
            || parts[2].is_empty()
            || parts[3..] != ["storage", "goog4_request"]
        {
            return Err(Failure::Malformed);
        }
        let date = required("X-Goog-Date")?;
        let active = parse_timestamp(date).map_err(|_| Failure::Malformed)?;
        if parts[1] != &date[..8] {
            return Err(Failure::Malformed);
        }
        let raw_expires = required("X-Goog-Expires")?;
        let expires = raw_expires
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=MAX_EXPIRES).contains(n))
            .filter(|_| raw_expires.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(Failure::Malformed)?;
        let signed_headers = required("X-Goog-SignedHeaders")?;
        let bytes = unhex(required("X-Goog-Signature")?)?;
        let canonical = canonical_request(method, uri, headers, &query, signed_headers)?;
        let index = registry
            .identities
            .iter()
            .position(|id| id.id == parts[0])
            .ok_or(Failure::UnknownSigner)?;
        let keys = registry
            .signing_keys
            .get(parts[0])
            .ok_or(Failure::UnknownSigner)?;
        let message = string_to_sign(date, &parts[1..].join("/"), &canonical);
        if !keys
            .iter()
            .any(|key| key.verify(message.as_bytes(), &bytes))
        {
            return Err(Failure::Mismatch);
        }
        let now = clock.now();
        // Google documents query signatures as usable 15 minutes before the active datetime.
        if now
            .checked_add(Duration::from_secs(900))
            .is_none_or(|now| now < active)
        {
            return Err(Failure::NotYetValid);
        }
        if now
            >= active
                .checked_add(Duration::from_secs(expires))
                .ok_or(Failure::Malformed)?
        {
            return Err(Failure::Expired);
        }
        Ok(Some(Caller::Principal(&registry.identities[index])))
    }
}

fn canonical_request(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    query: &BTreeMap<String, String>,
    signed: &str,
) -> Result<String, Failure> {
    let names: Vec<_> = signed.split(';').collect();
    if !names.contains(&"host")
        || names.windows(2).any(|w| w[0] >= w[1])
        || names.iter().any(|name| {
            name.is_empty()
                || *name != name.to_ascii_lowercase()
                || hyper::header::HeaderName::from_bytes(name.as_bytes()).is_err()
        })
    {
        return Err(Failure::Malformed);
    }
    for key in headers.keys().map(|key| key.as_str()) {
        if (key.starts_with("x-goog-") || key.starts_with("x-amz-"))
            && !matches!(key, "x-goog-content-sha256" | "x-amz-content-sha256")
            && !names.contains(&key)
        {
            return Err(Failure::Malformed);
        }
    }
    let mut canonical_headers = String::new();
    for name in names {
        let values = headers
            .get_all(name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(|v| v.split_ascii_whitespace().collect::<Vec<_>>().join(" "))
                    .map_err(|_| Failure::Malformed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.is_empty() || (name == "host" && (values.len() != 1 || values[0].is_empty())) {
            return Err(Failure::Mismatch);
        }
        if name == "host"
            && uri
                .authority()
                .is_some_and(|authority| authority.as_str() != values[0])
        {
            return Err(Failure::Mismatch);
        }
        writeln!(canonical_headers, "{name}:{}", values.join(",")).unwrap();
    }
    let path = canonical_path(uri.path())?;
    payload_hash(headers)?;
    // Query signatures always use UNSIGNED-PAYLOAD; a declared digest is checked by the adapter.
    Ok(format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{signed}\nUNSIGNED-PAYLOAD",
        canonical_query(query)
    ))
}

pub(crate) fn payload_hash(headers: &HeaderMap) -> Result<Option<[u8; 32]>, Failure> {
    match headers.get("x-goog-content-sha256") {
        None => Ok(None),
        Some(value) => {
            let value = value.to_str().map_err(|_| Failure::Malformed)?;
            if headers.get_all("x-goog-content-sha256").iter().count() != 1
                || (value != "UNSIGNED-PAYLOAD"
                    && (value.len() != 64
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))))
            {
                return Err(Failure::Malformed);
            }
            if value == "UNSIGNED-PAYLOAD" {
                Ok(None)
            } else {
                Ok(Some(
                    unhex(value)?.try_into().map_err(|_| Failure::Malformed)?,
                ))
            }
        }
    }
}

fn canonical_path(raw: &str) -> Result<String, Failure> {
    let mut result = String::new();
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let a = bytes
                .next()
                .filter(u8::is_ascii_hexdigit)
                .ok_or(Failure::Malformed)?;
            let b = bytes
                .next()
                .filter(u8::is_ascii_hexdigit)
                .ok_or(Failure::Malformed)?;
            result.push('%');
            result.push(a as char);
            result.push(b as char);
        } else if byte == b'/' || unreserved(byte) {
            result.push(byte as char);
        } else {
            write!(result, "%{byte:02X}").unwrap();
        }
    }
    Ok(result)
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn encode(raw: &str) -> String {
    let mut result = String::new();
    for byte in raw.bytes() {
        if unreserved(byte) {
            result.push(byte as char);
        } else {
            write!(result, "%{byte:02X}").unwrap();
        }
    }
    result
}

fn canonical_query(query: &BTreeMap<String, String>) -> String {
    query
        .iter()
        .filter(|(k, _)| k.as_str() != "X-Goog-Signature")
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn string_to_sign(date: &str, scope: &str, canonical: &str) -> String {
    format!(
        "{ALGORITHM}\n{date}\n{scope}\n{}",
        hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}

fn unhex(raw: &str) -> Result<Vec<u8>, Failure> {
    if raw.is_empty()
        || raw.len() > 2048
        || !raw.len().is_multiple_of(2)
        || !raw.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Failure::Malformed);
    }
    (0..raw.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).map_err(|_| Failure::Malformed))
        .collect()
}

/// Parse an exact UTC basic timestamp, validating the Gregorian date by round trip.
pub fn parse_timestamp(raw: &str) -> Result<SystemTime, String> {
    let invalid = || "timestamp must be a valid YYYYMMDDTHHMMSSZ UTC date".to_owned();
    if raw.len() != 16
        || raw.as_bytes()[8] != b'T'
        || raw.as_bytes()[15] != b'Z'
        || !raw
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || i == 15 || b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let n = |range: std::ops::Range<usize>| raw[range].parse::<i64>().map_err(|_| invalid());
    let (year, month, day) = (n(0..4)?, n(4..6)?, n(6..8)?);
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    let seconds = days * 86400 + n(9..11)? * 3600 + n(11..13)? * 60 + n(13..15)?;
    let time = if seconds >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    }
    .ok_or_else(invalid)?;
    if timestamp(time)? != raw {
        return Err(invalid());
    }
    Ok(time)
}

fn timestamp(now: SystemTime) -> Result<String, String> {
    let date = json::timestamp(now).map_err(|_| "timestamp out of range")?;
    Ok(format!(
        "{}{}{}T{}{}{}Z",
        &date[..4],
        &date[5..7],
        &date[8..10],
        &date[11..13],
        &date[14..16],
        &date[17..19]
    ))
}

/// Sign the exact final URL, including its authority and optional query, using a local PEM key.
pub fn sign_url(
    url: &str,
    method: &Method,
    signer: &str,
    private_key: &[u8],
    expires: u64,
    now: SystemTime,
    mut headers: HeaderMap,
) -> Result<String, String> {
    let mut uri: Uri = url.parse().map_err(|_| "invalid signing URL")?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || url.contains('#')
        || !matches!(*method, Method::GET | Method::HEAD | Method::PUT)
        || !super::visible_ascii(signer, 254)
        || signer.contains('/')
        || !(1..=MAX_EXPIRES).contains(&expires)
        || private_key.len() > 16384
    {
        return Err(
            "signing requires an HTTP(S) URL, GET/HEAD/PUT, a signer ID and 1..604800 seconds"
                .into(),
        );
    }
    if headers.contains_key("host") || headers.contains_key("authorization") {
        return Err("host and authorization cannot be supplied as signing headers".into());
    }
    let default_port = if uri.scheme_str() == Some("http") {
        80
    } else {
        443
    };
    if uri.authority().unwrap().port_u16() == Some(default_port) {
        // Browsers omit default ports in Host; normalize before signing the final URL.
        let authority = uri
            .authority()
            .unwrap()
            .as_str()
            .rsplit_once(':')
            .unwrap()
            .0
            .parse()
            .map_err(|_| "invalid host")?;
        let mut parts = uri.into_parts();
        parts.authority = Some(authority);
        uri = Uri::from_parts(parts).map_err(|_| "invalid signing URL")?;
    }
    headers.insert(
        "host",
        uri.authority()
            .unwrap()
            .as_str()
            .parse()
            .map_err(|_| "invalid host")?,
    );
    let date = timestamp(now)?;
    let scope = format!("{}/auto/storage/goog4_request", &date[..8]);
    let mut query = json::parse_query(uri.query().unwrap_or_default()).map_err(|e| e.message)?;
    if query
        .keys()
        .any(|k| PARAMETERS.iter().any(|p| k.eq_ignore_ascii_case(p)))
    {
        return Err("URL already contains signing parameters".into());
    }
    let mut names: Vec<_> = headers.keys().map(|h| h.as_str()).collect();
    names.sort();
    let signed = names.join(";");
    for (k, v) in [
        ("X-Goog-Algorithm", ALGORITHM.to_owned()),
        ("X-Goog-Credential", format!("{signer}/{scope}")),
        ("X-Goog-Date", date.clone()),
        ("X-Goog-Expires", expires.to_string()),
        ("X-Goog-SignedHeaders", signed.clone()),
    ] {
        query.insert(k.into(), v);
    }
    let canonical = canonical_request(method, &uri, &headers, &query, &signed)
        .map_err(|_| "invalid signing request")?;
    let message = string_to_sign(&date, &scope, &canonical);
    let key = PrivateKeyDer::from_pem_slice(private_key)
        .map_err(|_| "private key must be RSA PKCS#8 or PKCS#1 PEM")?;
    let key = match key {
        PrivateKeyDer::Pkcs8(key) => signature::RsaKeyPair::from_pkcs8(key.secret_pkcs8_der()),
        PrivateKeyDer::Pkcs1(key) => signature::RsaKeyPair::from_der(key.secret_pkcs1_der()),
        _ => return Err("private key must be RSA PKCS#8 or PKCS#1 PEM".into()),
    }
    .map_err(|e| format!("invalid RSA private key: {e}"))?;
    let mut bytes = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        message.as_bytes(),
        &mut bytes,
    )
    .map_err(|_| "RSA signing failed")?;
    Ok(format!(
        "{}://{}{}?{}&X-Goog-Signature={}",
        uri.scheme_str().unwrap(),
        uri.authority().unwrap(),
        uri.path(),
        canonical_query(&query),
        hex(&bytes)
    ))
}
