//! V4 wire parsing shared by Google RSA, Google HMAC and interoperable AWS HMAC.

pub(super) use super::verify::{Algorithm, Prepared};
use super::*;

#[cfg(test)]
mod tests;

impl Algorithm {
    fn parse(raw: &str) -> Result<Self, Failure> {
        match raw {
            ALGORITHM => Ok(Self::Rsa),
            "GOOG4-HMAC-SHA256" => Ok(Self::GoogleHmac),
            "AWS4-HMAC-SHA256" => Ok(Self::AwsHmac),
            _ => Err(Failure::Malformed),
        }
    }

    fn parameter_prefix(self) -> &'static str {
        if self == Self::AwsHmac {
            "X-Amz-"
        } else {
            "X-Goog-"
        }
    }

    /// The algorithm's name in a V4 string to sign; the legacy V2 algorithm has none.
    fn name(self) -> Result<&'static str, Failure> {
        match self {
            Self::Rsa => Ok(ALGORITHM),
            Self::GoogleHmac => Ok("GOOG4-HMAC-SHA256"),
            Self::AwsHmac => Ok("AWS4-HMAC-SHA256"),
            Self::LegacyHmacSha1 => Err(Failure::Malformed),
        }
    }

    fn expires_parameter(self) -> ExpiresParameter {
        match self {
            Self::AwsHmac => ExpiresParameter::Aws,
            Self::Rsa | Self::GoogleHmac | Self::LegacyHmacSha1 => ExpiresParameter::Google,
        }
    }
}

pub(crate) fn has_header(headers: &HeaderMap) -> bool {
    headers.get_all("authorization").iter().any(|value| {
        value.as_bytes().starts_with(b"GOOG4") || value.as_bytes().starts_with(b"AWS4")
    })
}

/// Connection admission bounds waiters; these caps also bound direct adapter callers.
pub(super) fn bounded(uri: &Uri, headers: &HeaderMap) -> Result<(), Failure> {
    if uri
        .path_and_query()
        .is_some_and(|value| value.as_str().len() > 16 * 1024)
        || headers.len() > 64
        || headers
            .iter()
            .try_fold(0usize, |size, (name, value)| {
                size.checked_add(name.as_str().len())?
                    .checked_add(value.as_bytes().len())
            })
            .is_none_or(|size| size > 64 * 1024)
    {
        return Err(Failure::Malformed);
    }
    Ok(())
}

pub(super) struct CredentialScope<'a> {
    pub signer: &'a str,
    pub scope: &'a str,
}

impl<'a> CredentialScope<'a> {
    pub(super) fn parse(raw: &'a str, date: &str, service: [&str; 2]) -> Result<Self, Failure> {
        let (signer, scope) = raw.split_once('/').ok_or(Failure::Malformed)?;
        let day = date.get(..8).ok_or(Failure::Malformed)?;
        let mut parts = scope.split('/');
        let [service, request] = service;
        let valid = !signer.is_empty()
            && parts.next() == Some(day)
            && parts.next().is_some_and(|region| !region.is_empty())
            && parts.next() == Some(service)
            && parts.next() == Some(request)
            && parts.next().is_none();
        valid
            .then_some(Self { signer, scope })
            .ok_or(Failure::Malformed)
    }
}

pub(super) fn prepare(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    now: SystemTime,
) -> Result<Prepared, Failure> {
    bounded(uri, headers)?;
    let query =
        json::parse_query(uri.query().unwrap_or_default()).map_err(|_| Failure::Malformed)?;
    let header = has_header(headers);
    if query
        .keys()
        .any(|name| signing_param(name.as_bytes()) == Some(SigningParams::V2))
        || (header && is_signed(uri.query()))
    {
        return Err(Failure::Malformed);
    }
    let (algorithm, credential, date, signed, signature, expires) = if header {
        header_fields(headers)?
    } else {
        if headers.contains_key("authorization") {
            return Err(Failure::Malformed);
        }
        query_fields(&query)?
    };
    let active = parse_timestamp(date).map_err(|_| Failure::Malformed)?;
    let expected = if algorithm == Algorithm::AwsHmac {
        ["s3", "aws4_request"]
    } else {
        ["storage", "goog4_request"]
    };
    let credential = CredentialScope::parse(credential, date, expected)?;
    if now
        .checked_add(Duration::from_secs(900))
        .is_none_or(|now| now < active)
    {
        return Err(Failure::NotYetValid);
    }
    let end = active
        .checked_add(Duration::from_secs(expires))
        .ok_or(Failure::Malformed)?;
    if now > end || (!header && now == end) {
        return Err(Failure::Expired);
    }
    supported_request(
        method,
        headers,
        if header {
            Signing::Header
        } else {
            Signing::Query
        },
        signed,
    )?;
    let streaming = super::streaming::validate(
        method,
        headers,
        algorithm,
        if header {
            Signing::Header
        } else {
            Signing::Query
        },
    )?;
    let opposite = if algorithm == Algorithm::AwsHmac {
        ["x-goog-date", "x-goog-content-sha256"]
    } else {
        ["x-amz-date", "x-amz-content-sha256"]
    };
    if opposite.iter().any(|name| headers.contains_key(*name)) {
        return Err(Failure::Malformed);
    }
    let hash = if streaming {
        None
    } else {
        payload_hash(headers)?
    };
    if header
        && !headers.contains_key(if algorithm == Algorithm::AwsHmac {
            "x-amz-content-sha256"
        } else {
            "x-goog-content-sha256"
        })
    {
        return Err(Failure::Malformed);
    }
    let payload = if streaming {
        STREAMING_PAYLOAD.into()
    } else if header {
        hash.map(|hash| encode_lower(&hash))
            .unwrap_or_else(|| "UNSIGNED-PAYLOAD".into())
    } else {
        "UNSIGNED-PAYLOAD".into()
    };
    let name = algorithm.name()?;
    let scope = credential.scope;
    let canonical =
        |host| canonical_with_payload(method, uri, headers, &query, signed, &payload, host);
    let as_sent = canonical(CanonicalHost::AsSent)?;
    // Go storage drops an explicit port from the canonical host. A mismatch names that cause,
    // and a lenient verifier retries with this form.
    let without_port = canonical(CanonicalHost::WithoutPort)?;
    Ok(Prepared {
        signer: credential.signer.into(),
        date: date.into(),
        algorithm,
        scope: scope.to_owned(),
        message: string_to_sign(name, date, scope, &as_sent),
        portless: (without_port != as_sent)
            .then(|| string_to_sign(name, date, scope, &without_port)),
        signature: unhex(signature)?,
    })
}

#[derive(Clone, Copy)]
pub(super) enum Signing {
    Query,
    Header,
}

fn supported_request(
    method: &Method,
    headers: &HeaderMap,
    signing: Signing,
    signed: &str,
) -> Result<(), Failure> {
    let resumable = resumable_start(headers)?;
    // Google accepts a signed URL POST only to start a resumable upload. A URL signed for
    // that purpose but sent without the header reaches canonicalization and fails as a
    // signature mismatch, because the signed header is missing.
    let query_post = matches!(signing, Signing::Query) && *method == Method::POST;
    if !matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::POST
    ) || (query_post && !resumable && !signed.split(';').any(|name| name == "x-goog-resumable"))
        || (resumable && *method != Method::POST)
        || headers.contains_key("x-amz-trailer")
        || headers.contains_key("trailer")
    {
        return Err(Failure::Malformed);
    }
    Ok(())
}

type Fields<'a> = (Algorithm, &'a str, &'a str, &'a str, &'a str, u64);

fn query_fields(query: &BTreeMap<String, String>) -> Result<Fields<'_>, Failure> {
    let google = query.get("X-Goog-Algorithm");
    let aws = query.get("X-Amz-Algorithm");
    let algorithm = Algorithm::parse(google.or(aws).ok_or(Failure::Malformed)?)?;
    let prefix = algorithm.parameter_prefix();
    if query.keys().any(|name| {
        signing_param(name.as_bytes()).is_some()
            && (!name.starts_with(prefix)
                || !(PARAMETERS.contains(&name.as_str())
                    || AWS_PARAMETERS.contains(&name.as_str())))
    }) {
        return Err(Failure::Malformed);
    }
    let required = |suffix| {
        query
            .get(&format!("{prefix}{suffix}"))
            .filter(|value| !value.is_empty())
            .map(String::as_str)
            .ok_or(Failure::Malformed)
    };
    let raw = required("Expires")?;
    let expires = raw
        .parse::<u64>()
        .ok()
        .filter(|n| (1..=MAX_EXPIRES).contains(n))
        .filter(|_| raw.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(Failure::ExpiresOutOfRange(algorithm.expires_parameter()))?;
    Ok((
        algorithm,
        required("Credential")?,
        required("Date")?,
        required("SignedHeaders")?,
        required("Signature")?,
        expires,
    ))
}

fn header_fields(headers: &HeaderMap) -> Result<Fields<'_>, Failure> {
    let mut values = headers.get_all("authorization").iter();
    let raw = values
        .next()
        .ok_or(Failure::Malformed)?
        .to_str()
        .map_err(|_| Failure::Malformed)?;
    if values.next().is_some() {
        return Err(Failure::Malformed);
    }
    let (algorithm, fields) = raw.split_once(' ').ok_or(Failure::Malformed)?;
    let algorithm = Algorithm::parse(algorithm)?;
    let mut parsed = BTreeMap::new();
    for field in fields.split(',') {
        let (name, value) = field.trim().split_once('=').ok_or(Failure::Malformed)?;
        if !matches!(name, "Credential" | "SignedHeaders" | "Signature")
            || value.is_empty()
            || parsed.insert(name, value).is_some()
        {
            return Err(Failure::Malformed);
        }
    }
    let required = |name| parsed.get(name).copied().ok_or(Failure::Malformed);
    let name = if algorithm == Algorithm::AwsHmac {
        "x-amz-date"
    } else {
        "x-goog-date"
    };
    let mut dates = headers.get_all(name).iter();
    let date = dates
        .next()
        .ok_or(Failure::Malformed)?
        .to_str()
        .map_err(|_| Failure::Malformed)?;
    if dates.next().is_some() {
        return Err(Failure::Malformed);
    }
    Ok((
        algorithm,
        required("Credential")?,
        date,
        required("SignedHeaders")?,
        required("Signature")?,
        900,
    ))
}
