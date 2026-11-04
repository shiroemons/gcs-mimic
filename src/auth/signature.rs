//! Bounded, local V2/V4 XML signature verification and RSA signing. No Google IAM calls.

use super::{Auth, Caller};
use crate::{
    clock::Clock,
    hashing::encode_lower,
    json,
    url::{is_unreserved, push_percent_encoded},
};
use hyper::{HeaderMap, Method, Uri};
use ring::{digest, rand::SystemRandom, signature};
use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, SubjectPublicKeyInfoDer, pem::PemObject};

pub(crate) const PARAMETERS: &[&str] = &[
    "X-Goog-Algorithm",
    "X-Goog-Credential",
    "X-Goog-Date",
    "X-Goog-Expires",
    "X-Goog-SignedHeaders",
    "X-Goog-Signature",
];
pub(crate) const AWS_PARAMETERS: &[&str] = &[
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-Expires",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
];
const ALGORITHM: &str = "GOOG4-RSA-SHA256";
pub(crate) const MAX_EXPIRES: u64 = 604800;
pub(crate) const V2_PARAMETERS: &[&str] = &["GoogleAccessId", "Expires", "Signature"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SigningParams {
    GoogleV4,
    AwsV4,
    V2,
}

impl SigningParams {
    fn names(self) -> &'static [&'static str] {
        match self {
            Self::GoogleV4 => PARAMETERS,
            Self::AwsV4 => AWS_PARAMETERS,
            Self::V2 => V2_PARAMETERS,
        }
    }
}

/// One decoded-name predicate keeps dispatch, parsing and signing on the same families.
pub(crate) fn signing_param(name: &[u8]) -> Option<SigningParams> {
    [
        SigningParams::GoogleV4,
        SigningParams::AwsV4,
        SigningParams::V2,
    ]
    .into_iter()
    .find(|set| {
        set.names()
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate.as_bytes()))
    })
}

pub(crate) enum Resource {
    PathStyle,
    VirtualHosted(String),
}

mod policy;
mod streaming;
mod v2;
mod v4;
mod verify;
pub(crate) use streaming::{STREAMING_PAYLOAD, Streaming};
pub(crate) use v2::{legacy_aws, xml_query};
pub(crate) use verify::VerificationError;
pub(crate) use verify::Verifiers;
pub(crate) use verify::has_header;

pub(crate) fn authentication_header(name: &str) -> bool {
    matches!(
        name,
        "x-goog-date" | "x-amz-date" | "x-goog-content-sha256" | "x-amz-content-sha256"
    )
}

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

/// A `public_keys` entry that is not a supported RSA SubjectPublicKeyInfo PEM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidPublicKey;

impl fmt::Display for InvalidPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("public_keys must contain only RSA SubjectPublicKeyInfo PEM (2048..8192 bits)")
    }
}

impl std::error::Error for InvalidPublicKey {}

/// Why a timestamp could not be parsed or a URL or policy could not be signed. `Display` is the
/// operator-facing message printed by the `url sign` command.
#[derive(Clone, Debug)]
pub enum SigningError {
    InvalidTimestamp,
    TimestampOutOfRange,
    InvalidUrl,
    UnsupportedRequest,
    ReservedHeader,
    InvalidHost,
    InvalidQuery(String),
    ExistingSigningParameters,
    InvalidRequest,
    PrivateKeyTooLarge,
    UnsupportedPrivateKey,
    /// ring's reason for rejecting the key.
    InvalidPrivateKey(ring::error::KeyRejected),
    SigningFailed,
}

impl fmt::Display for SigningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimestamp => {
                f.write_str("timestamp must be a valid YYYYMMDDTHHMMSSZ UTC date")
            }
            Self::TimestampOutOfRange => f.write_str("timestamp out of range"),
            Self::InvalidUrl => f.write_str("invalid signing URL"),
            Self::UnsupportedRequest => f.write_str(
                "signing requires an HTTP(S) URL, GET/HEAD/PUT/DELETE or POST with x-goog-resumable:start, a signer ID and 1..604800 seconds",
            ),
            Self::ReservedHeader => {
                f.write_str("host and authorization cannot be supplied as signing headers")
            }
            Self::InvalidHost => f.write_str("invalid host"),
            Self::InvalidQuery(message) => f.write_str(message),
            Self::ExistingSigningParameters => {
                f.write_str("URL already contains signing parameters")
            }
            Self::InvalidRequest => f.write_str("invalid signing request"),
            Self::PrivateKeyTooLarge => f.write_str("private key exceeds 16 KiB"),
            Self::UnsupportedPrivateKey => {
                f.write_str("private key must be RSA PKCS#8 or PKCS#1 PEM")
            }
            Self::InvalidPrivateKey(reason) => write!(f, "invalid RSA private key: {reason}"),
            Self::SigningFailed => f.write_str("RSA signing failed"),
        }
    }
}

// ring's `KeyRejected` implements `std::error::Error` only with ring's `std` feature, which
// this crate does not enable, so `InvalidPrivateKey` keeps it typed but cannot expose it as
// `source()`.
impl std::error::Error for SigningError {}

#[derive(Clone, Debug)]
pub(super) struct PublicKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

impl PublicKey {
    pub(super) fn parse(pem: &str) -> Result<Self, InvalidPublicKey> {
        let invalid = || InvalidPublicKey;
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
        let mut bits = bits.strip_prefix(&[0]).ok_or_else(invalid)?;
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

    pub(super) fn verify(&self, message: &[u8], bytes: &[u8]) -> bool {
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
    *input = input.get(start + len..)?;
    Some(value)
}

fn integer<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let value = element(input, 0x02)?;
    match value {
        [0, rest @ ..] if rest.first().is_some_and(|next| next & 128 != 0) => Some(rest),
        [first, ..] if *first > 0 && first & 128 == 0 => Some(value),
        _ => None,
    }
}

/// Whether a request starts an XML resumable upload. `start` is the only documented value, and
/// one header value keeps the signed canonical form unambiguous.
pub(crate) fn resumable_start(headers: &HeaderMap) -> Result<bool, Failure> {
    let mut values = headers.get_all("x-goog-resumable").iter();
    let Some(value) = values.next() else {
        return Ok(false);
    };
    if values.next().is_some() || value != "start" {
        return Err(Failure::Malformed);
    }
    Ok(true)
}

pub(crate) fn is_signed(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        query.split('&').any(|pair| {
            let key = super::lenient_decode(pair.split('=').next().unwrap_or_default());
            signing_param(&key).is_some()
        })
    })
}

fn canonical_with_payload(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    query: &BTreeMap<String, String>,
    signed: &str,
    payload: &str,
) -> Result<String, Failure> {
    let names: Vec<_> = signed.split(';').collect();
    if !names.contains(&"host")
        || !names.is_sorted_by(|a, b| a < b)
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
        let Some(first) = values.first() else {
            return Err(Failure::Mismatch);
        };
        if name == "host" && (values.len() != 1 || first.is_empty()) {
            return Err(Failure::Mismatch);
        }
        if name == "host"
            && uri
                .authority()
                .is_some_and(|authority| authority.as_str() != first)
        {
            return Err(Failure::Mismatch);
        }
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(&values.join(","));
        canonical_headers.push('\n');
    }
    let path = canonical_path(uri.path())?;
    Ok(format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{signed}\n{payload}",
        canonical_query(query)
    ))
}

pub(crate) fn payload_hash(headers: &HeaderMap) -> Result<Option<[u8; 32]>, Failure> {
    let name = if headers.contains_key("x-amz-content-sha256") {
        if headers.contains_key("x-goog-content-sha256") {
            return Err(Failure::Malformed);
        }
        "x-amz-content-sha256"
    } else {
        "x-goog-content-sha256"
    };
    match headers.get(name) {
        None => Ok(None),
        Some(value) => {
            let value = value.to_str().map_err(|_| Failure::Malformed)?;
            if headers.get_all(name).iter().count() != 1
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
        } else if byte == b'/' || is_unreserved(byte) {
            result.push(byte as char);
        } else {
            push_percent_encoded(&mut result, byte);
        }
    }
    Ok(result)
}

/// Encode a literal query name or value using RFC 3986 unreserved bytes.
pub fn encode_query_component(raw: &str) -> String {
    crate::url::encode_unreserved(raw)
}

fn canonical_query(query: &BTreeMap<String, String>) -> String {
    query
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "X-Goog-Signature" | "X-Amz-Signature"))
        .map(|(k, v)| {
            format!(
                "{}={}",
                encode_query_component(k),
                encode_query_component(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn string_to_sign(date: &str, scope: &str, canonical: &str) -> String {
    format!(
        "{ALGORITHM}\n{date}\n{scope}\n{}",
        encode_lower(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
    )
}

fn unhex(raw: &str) -> Result<Vec<u8>, Failure> {
    if raw.is_empty()
        || raw.len() > 2048
        || !raw.len().is_multiple_of(2)
        || !raw.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Failure::Malformed);
    }
    raw.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| crate::hex::byte(pair).ok_or(Failure::Malformed))
        .collect()
}

/// Parse an exact UTC basic timestamp, validating the Gregorian date by round trip.
pub fn parse_timestamp(raw: &str) -> Result<SystemTime, SigningError> {
    let invalid = || SigningError::InvalidTimestamp;
    if raw.len() != 16
        || raw.as_bytes().get(8) != Some(&b'T')
        || raw.as_bytes().get(15) != Some(&b'Z')
        || !raw
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || i == 15 || b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let n = |range: std::ops::Range<usize>| {
        raw.get(range)
            .ok_or_else(invalid)?
            .parse::<i64>()
            .map_err(|_| invalid())
    };
    let (year, month, day) = (n(0..4)?, n(4..6)?, n(6..8)?);
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    let seconds = days * 86400 + n(9..11)? * 3600 + n(11..13)? * 60 + n(13..15)?;
    let time = crate::clock::system_time_from_unix_seconds(seconds).ok_or_else(invalid)?;
    if timestamp(time)? != raw {
        return Err(invalid());
    }
    Ok(time)
}

/// Format a time as the `YYYYMMDDTHHMMSSZ` value of X-Goog-Date and x-goog-date.
pub fn timestamp(now: SystemTime) -> Result<String, SigningError> {
    let date = json::timestamp(now).map_err(|_| SigningError::TimestampOutOfRange)?;
    let part = |range| date.get(range).ok_or(SigningError::TimestampOutOfRange);
    Ok(format!(
        "{}{}{}T{}{}{}Z",
        part(0..4)?,
        part(5..7)?,
        part(8..10)?,
        part(11..13)?,
        part(14..16)?,
        part(17..19)?
    ))
}

/// The final URL and local credentials bound by a V4 signature.
pub struct SignUrlRequest<'a> {
    pub url: &'a str,
    pub method: &'a Method,
    pub signer: &'a str,
    pub private_key: &'a [u8],
    pub expires: u64,
    pub now: SystemTime,
    pub headers: HeaderMap,
}

/// Sign the exact final URL, including its authority and optional query, using a local PEM key.
pub fn sign_url(request: SignUrlRequest<'_>) -> Result<String, SigningError> {
    let SignUrlRequest {
        url,
        method,
        signer,
        private_key,
        expires,
        now,
        mut headers,
    } = request;
    let mut uri: Uri = url.parse().map_err(|_| SigningError::InvalidUrl)?;
    let (Some(scheme), Some(mut authority)) = (uri.scheme().cloned(), uri.authority().cloned())
    else {
        return Err(SigningError::UnsupportedRequest);
    };
    if !matches!(scheme.as_str(), "http" | "https")
        || authority.as_str().contains('@')
        || url.contains('#')
        || !(matches!(
            *method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE
        ) || (*method == Method::POST && resumable_start(&headers) == Ok(true)))
        || !super::visible_ascii(signer, 254)
        || signer.contains('/')
        || !(1..=MAX_EXPIRES).contains(&expires)
        || private_key.len() > 16384
    {
        return Err(SigningError::UnsupportedRequest);
    }
    if headers.contains_key("host") || headers.contains_key("authorization") {
        return Err(SigningError::ReservedHeader);
    }
    let default_port = if scheme.as_str() == "http" { 80 } else { 443 };
    if authority.port_u16() == Some(default_port) {
        // Browsers omit default ports in Host; normalize before signing the final URL.
        authority = authority
            .as_str()
            .rsplit_once(':')
            .map_or(authority.as_str(), |(host, _)| host)
            .parse()
            .map_err(|_| SigningError::InvalidHost)?;
        let mut parts = uri.into_parts();
        parts.authority = Some(authority.clone());
        uri = Uri::from_parts(parts).map_err(|_| SigningError::InvalidUrl)?;
    }
    headers.insert(
        "host",
        authority
            .as_str()
            .parse()
            .map_err(|_| SigningError::InvalidHost)?,
    );
    let date = timestamp(now)?;
    let day = date.get(..8).ok_or(SigningError::TimestampOutOfRange)?;
    let scope = format!("{day}/auto/storage/goog4_request");
    let mut query = json::parse_query(uri.query().unwrap_or_default())
        .map_err(|e| SigningError::InvalidQuery(e.message))?;
    if query
        .keys()
        .any(|key| signing_param(key.as_bytes()).is_some())
    {
        return Err(SigningError::ExistingSigningParameters);
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
    payload_hash(&headers).map_err(|_| SigningError::InvalidRequest)?;
    let canonical =
        canonical_with_payload(method, &uri, &headers, &query, &signed, "UNSIGNED-PAYLOAD")
            .map_err(|_| SigningError::InvalidRequest)?;
    let message = string_to_sign(&date, &scope, &canonical);
    let bytes = rsa_sign(private_key, &message)?;
    Ok(format!(
        "{scheme}://{authority}{}?{}&X-Goog-Signature={}",
        uri.path(),
        canonical_query(&query),
        encode_lower(&bytes)
    ))
}

/// Sign a base64 V4 POST policy document the way the official SDKs do, returning hex.
pub fn sign_policy(policy: &str, private_key: &[u8]) -> Result<String, SigningError> {
    if private_key.len() > 16384 {
        return Err(SigningError::PrivateKeyTooLarge);
    }
    rsa_sign(private_key, policy).map(|bytes| encode_lower(&bytes))
}

fn rsa_sign(private_key: &[u8], message: &str) -> Result<Vec<u8>, SigningError> {
    let key = PrivateKeyDer::from_pem_slice(private_key)
        .map_err(|_| SigningError::UnsupportedPrivateKey)?;
    let key = match key {
        PrivateKeyDer::Pkcs8(key) => signature::RsaKeyPair::from_pkcs8(key.secret_pkcs8_der()),
        PrivateKeyDer::Pkcs1(key) => signature::RsaKeyPair::from_der(key.secret_pkcs1_der()),
        _ => return Err(SigningError::UnsupportedPrivateKey),
    }
    .map_err(SigningError::InvalidPrivateKey)?;
    let mut bytes = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        message.as_bytes(),
        &mut bytes,
    )
    .map_err(|_| SigningError::SigningFailed)?;
    Ok(bytes)
}
