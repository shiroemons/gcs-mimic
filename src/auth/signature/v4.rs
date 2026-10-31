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
        .any(|name| V2_PARAMETERS.iter().any(|p| name.eq_ignore_ascii_case(p)))
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
    let parts: Vec<_> = credential.split('/').collect();
    let expected = if algorithm == Algorithm::AwsHmac {
        ["s3", "aws4_request"]
    } else {
        ["storage", "goog4_request"]
    };
    if parts.len() != 5
        || parts[0].is_empty()
        || parts[2].is_empty()
        || parts[1] != &date[..8]
        || parts[3..] != expected
    {
        return Err(Failure::Malformed);
    }
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
        "x-goog-"
    } else {
        "x-amz-"
    };
    if headers.contains_key(format!("{opposite}date"))
        || headers.contains_key(format!("{opposite}content-sha256"))
    {
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
    let canonical = canonical_with_payload(method, uri, headers, &query, signed, &payload)?;
    let scope = parts[1..].join("/");
    let algorithm_name = match algorithm {
        Algorithm::Rsa => ALGORITHM,
        Algorithm::GoogleHmac => "GOOG4-HMAC-SHA256",
        Algorithm::AwsHmac => "AWS4-HMAC-SHA256",
        Algorithm::LegacyHmacSha1 => return Err(Failure::Malformed),
    };
    let message = format!(
        "{algorithm_name}\n{date}\n{scope}\n{}",
        encode_lower(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
    );
    Ok(Prepared {
        signer: parts[0].into(),
        algorithm,
        scope,
        message,
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
) -> Result<(), Failure> {
    // Signed URL POST is restricted to resumable initiation, which this adapter does not offer.
    if !matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::POST
    ) || (matches!(signing, Signing::Query) && *method == Method::POST)
        || headers.contains_key("x-goog-resumable")
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
        PARAMETERS
            .iter()
            .chain(AWS_PARAMETERS)
            .any(|parameter| name.eq_ignore_ascii_case(parameter))
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
        .ok_or(Failure::Malformed)?;
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
