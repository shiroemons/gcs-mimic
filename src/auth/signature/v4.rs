//! V4 wire parsing shared by Google RSA, Google HMAC and interoperable AWS HMAC.

use super::streaming::PayloadFraming;
pub(super) use super::verify::Prepared;
use super::verify::{Scheme, V4Scope};
use super::*;
use std::str::FromStr;

#[cfg(test)]
mod tests;

/// Which naming of the V4 parameters, headers and key prefix a signer used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Family {
    Google,
    Aws,
}

impl Family {
    /// The prefix the V4 signing key puts before the secret.
    pub(super) const fn key_prefix(self) -> &'static str {
        match self {
            Self::Google => "GOOG4",
            Self::Aws => "AWS4",
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Google => Self::Aws,
            Self::Aws => Self::Google,
        }
    }

    const fn date_header(self) -> &'static str {
        match self {
            Self::Google => "x-goog-date",
            Self::Aws => "x-amz-date",
        }
    }

    const fn content_sha256_header(self) -> &'static str {
        match self {
            Self::Google => "x-goog-content-sha256",
            Self::Aws => "x-amz-content-sha256",
        }
    }

    const fn credential_service(self) -> [&'static str; 2] {
        match self {
            Self::Google => ["storage", "goog4_request"],
            Self::Aws => ["s3", "aws4_request"],
        }
    }
}

impl From<Family> for ExpiresParameter {
    fn from(family: Family) -> Self {
        match family {
            Family::Google => Self::Google,
            Family::Aws => Self::Aws,
        }
    }
}

crate::choice::string_enum! {
    table
    /// What a V4 query parameter carries: the part of its name after the family prefix.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Field as as_str {
        Algorithm => "algorithm",
        Credential => "credential",
        Date => "date",
        Expires => "expires",
        SignedHeaders => "signedheaders",
        Signature => "signature",
    };
    ascii_case(NotV4Parameter, NotV4Parameter)
}

/// A V4 signing query parameter. Its name parses in any ASCII case, because `gsutil signurl` and
/// `gcloud storage sign-url` send lowercase names. V2 names are not V4 parameters, and their
/// parser reads only the exact spellings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct V4Parameter {
    family: Family,
    field: Field,
}

impl V4Parameter {
    pub(super) fn is_signature(self) -> bool {
        self.field == Field::Signature
    }

    pub(super) fn is_aws(self) -> bool {
        self.family == Family::Aws
    }
}

/// The lowercase name Cloud Storage's error details use.
impl fmt::Display for V4Parameter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.family {
            Family::Google => "x-goog-",
            Family::Aws => "x-amz-",
        };
        let field = self.field.as_str();
        write!(formatter, "{prefix}{field}")
    }
}

/// A query name that does not identify a V4 signing parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a V4 signing query parameter")]
pub(crate) struct NotV4Parameter;

impl FromStr for V4Parameter {
    type Err = NotV4Parameter;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let (family, field) = [("X-Goog-", Family::Google), ("X-Amz-", Family::Aws)]
            .into_iter()
            .find_map(|(prefix, family)| {
                let (head, field) = name.split_at_checked(prefix.len())?;
                head.eq_ignore_ascii_case(prefix).then_some((family, field))
            })
            .ok_or(NotV4Parameter)?;
        Ok(Self {
            family,
            field: field.parse()?,
        })
    }
}

crate::choice::string_enum! {
    table
    /// A V4 signing algorithm.
    /// Its name is the first line of the V4 string to sign.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Algorithm as pub(super) name {
        Rsa => ALGORITHM,
        GoogleHmac => "GOOG4-HMAC-SHA256",
        AwsHmac => "AWS4-HMAC-SHA256",
    };
    from_str(Failure, _ => Err(Failure::Malformed))
}

impl Algorithm {
    pub(super) fn family(self) -> Family {
        match self {
            Self::AwsHmac => Family::Aws,
            Self::Rsa | Self::GoogleHmac => Family::Google,
        }
    }

    /// How a signature by this algorithm verifies, given its date and credential scope.
    pub(super) fn scheme(self, date: &str, scope: &str) -> Scheme {
        match self {
            Self::Rsa => Scheme::Rsa,
            Self::GoogleHmac | Self::AwsHmac => Scheme::HmacV4(V4Scope {
                family: self.family(),
                date: date.to_owned(),
                scope: scope.to_owned(),
            }),
        }
    }
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
    pub(crate) signer: &'a str,
    pub(crate) scope: &'a str,
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
    let query = crate::percent::parse_query(uri.query().unwrap_or_default())
        .map_err(|_| Failure::Malformed)?;
    let header = has_v4_header(headers);
    if query
        .keys()
        .any(|name| signing_param(name.as_bytes()) == Some(SigningParams::V2))
        || (header && is_signed(uri.query()))
    {
        return Err(Failure::Malformed);
    }
    let Fields {
        algorithm,
        credential,
        date,
        signed_headers,
        signature,
        signing,
    } = if header {
        Fields::try_from(headers)?
    } else {
        if headers.contains_key("authorization") {
            return Err(Failure::Malformed);
        }
        Fields::try_from(&query)?
    };
    let active = parse_timestamp(date).map_err(|_| Failure::Malformed)?;
    let family = algorithm.family();
    let credential = CredentialScope::parse(credential, date, family.credential_service())?;
    check_window(active, now, signing)?;
    // Everything from here fails as Malformed until the canonical request is rendered, so the
    // order of these checks never changes the answer; the time window above stays first.
    let signed = SignedHeaders::try_from(signed_headers)?;
    supported_request(method, headers, signing, &signed)?;
    let framing = super::streaming::validate(method, headers, algorithm, signing)?;
    let other = family.other();
    if [other.date_header(), other.content_sha256_header()]
        .iter()
        .any(|name| headers.contains_key(*name))
    {
        return Err(Failure::Malformed);
    }
    let payload = match (framing, signing) {
        (PayloadFraming::AwsChunked, _) => CanonicalPayload::Streaming,
        (PayloadFraming::Plain, Signing::Query { .. }) => {
            // A signed URL signs UNSIGNED-PAYLOAD, but a malformed declaration still fails.
            PayloadHash::of(headers)?;
            CanonicalPayload::Unsigned
        }
        // The other family's header was refused above, so an absent declaration means this
        // family's header is missing, which a header signature must send.
        (PayloadFraming::Plain, Signing::Header) => match PayloadHash::of(headers)? {
            PayloadHash::Sha256(hash) => CanonicalPayload::Sha256(hash),
            PayloadHash::Unsigned => CanonicalPayload::Unsigned,
            PayloadHash::Absent => return Err(Failure::Malformed),
        },
    };
    let name = algorithm.name();
    let scope = credential.scope;
    let request = CanonicalRequest {
        method,
        uri,
        headers,
        query: &query,
        signed: &signed,
        payload,
    };
    let as_sent = request.render(CanonicalHost::AsSent)?;
    // Go storage drops an explicit port from the canonical host. A signature that fails as sent
    // is checked against this form too: exact names the port as the cause, lenient accepts.
    let without_port = request.render(CanonicalHost::WithoutPort)?;
    Ok(Prepared {
        signer: credential.signer.into(),
        scheme: algorithm.scheme(date, scope),
        message: string_to_sign(name, date, scope, &as_sent),
        portless: (without_port != as_sent)
            .then(|| string_to_sign(name, date, scope, &without_port)),
        signature: unhex(signature)?,
    })
}

/// The signature lives from its date: `NotYetValid` beyond the clock skew ahead, `Expired` once
/// its lifetime ends.
fn check_window(active: SystemTime, now: SystemTime, signing: Signing) -> Result<(), Failure> {
    if now.checked_add(CLOCK_SKEW).is_none_or(|now| now < active) {
        return Err(Failure::NotYetValid);
    }
    let end = active
        .checked_add(signing.lifetime())
        .ok_or(Failure::Malformed)?;
    if signing.expired(now, end) {
        return Err(Failure::Expired);
    }
    Ok(())
}

/// Where a V4 signature travels, which sets how long it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Signing {
    /// Query parameters: the signature lives `expires` seconds, excluding the last instant.
    Query { expires: u64 },
    /// An `Authorization` value: it lives for the clock skew, including the last instant.
    Header,
}

impl Signing {
    fn lifetime(self) -> Duration {
        match self {
            Self::Query { expires } => Duration::from_secs(expires),
            Self::Header => CLOCK_SKEW,
        }
    }

    fn expired(self, now: SystemTime, end: SystemTime) -> bool {
        match self {
            Self::Query { .. } => now >= end,
            Self::Header => now > end,
        }
    }
}

fn supported_request(
    method: &Method,
    headers: &HeaderMap,
    signing: Signing,
    signed: &SignedHeaders<'_>,
) -> Result<(), Failure> {
    let resumable = Resumable::of(headers)?;
    // Google accepts a signed URL POST only to start a resumable upload. A URL signed for
    // that purpose but sent without the header reaches canonicalization and fails as a
    // signature mismatch, because the signed header is missing.
    let query_post = matches!(signing, Signing::Query { .. }) && *method == Method::POST;
    if !matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::POST
    ) || (query_post && resumable == Resumable::Absent && !signed.contains("x-goog-resumable"))
        || (resumable == Resumable::Start && *method != Method::POST)
        || headers.contains_key("x-amz-trailer")
        || headers.contains_key("trailer")
    {
        return Err(Failure::Malformed);
    }
    Ok(())
}

/// The V4 signing inputs of one request, all present once parsing succeeds.
struct Fields<'a> {
    algorithm: Algorithm,
    credential: &'a str,
    date: &'a str,
    signed_headers: &'a str,
    signature: &'a str,
    signing: Signing,
}

/// One value per field while a query is parsed; the parser's result is a complete `Fields`.
#[derive(Default)]
struct QueryValues<'a> {
    algorithm: Option<&'a str>,
    credential: Option<&'a str>,
    date: Option<&'a str>,
    expires: Option<&'a str>,
    signed_headers: Option<&'a str>,
    signature: Option<&'a str>,
}

impl<'a> QueryValues<'a> {
    /// Rejects a second value: spellings that differ only in case would all reach the canonical
    /// query while only one of them supplied the value.
    fn set(&mut self, parameter: V4Parameter, value: &'a str) -> Result<(), Failure> {
        let slot = match parameter.field {
            Field::Algorithm => &mut self.algorithm,
            Field::Credential => &mut self.credential,
            Field::Date => &mut self.date,
            Field::Expires => &mut self.expires,
            Field::SignedHeaders => &mut self.signed_headers,
            Field::Signature => &mut self.signature,
        };
        if slot.replace(value).is_some() {
            return Err(Failure::RepeatedParameter(parameter));
        }
        Ok(())
    }
}

fn non_empty(value: Option<&str>) -> Result<&str, Failure> {
    value
        .filter(|value| !value.is_empty())
        .ok_or(Failure::Malformed)
}

/// A lifetime in whole seconds within `1..=MAX_EXPIRES`.
fn parse_expires(raw: &str, parameter: ExpiresParameter) -> Result<u64, Failure> {
    let out_of_range = |value| Failure::ExpiresOutOfRange(parameter, value);
    match crate::decimal::Grammar::Digits.parse::<u64>(raw) {
        Some(seconds @ 1..=MAX_EXPIRES) => Ok(seconds),
        Some(0) => Err(out_of_range(InvalidExpires::Zero)),
        Some(_) | None => Err(out_of_range(InvalidExpires::Other)),
    }
}

impl<'a> TryFrom<&'a BTreeMap<String, String>> for Fields<'a> {
    type Error = Failure;

    fn try_from(query: &'a BTreeMap<String, String>) -> Result<Self, Self::Error> {
        let mut sent_family = None;
        let mut values = QueryValues::default();
        for (name, value) in query {
            let Ok(parameter) = name.parse::<V4Parameter>() else {
                continue;
            };
            if *sent_family.get_or_insert(parameter.family) != parameter.family {
                return Err(Failure::Malformed);
            }
            values.set(parameter, value)?;
        }
        let QueryValues {
            algorithm,
            credential,
            date,
            expires,
            signed_headers,
            signature,
        } = values;
        let algorithm: Algorithm = algorithm.ok_or(Failure::Malformed)?.parse()?;
        let family = algorithm.family();
        if sent_family != Some(family) {
            return Err(Failure::Malformed);
        }
        let expires = parse_expires(non_empty(expires)?, family.into())?;
        Ok(Self {
            algorithm,
            credential: non_empty(credential)?,
            date: non_empty(date)?,
            signed_headers: non_empty(signed_headers)?,
            signature: non_empty(signature)?,
            signing: Signing::Query { expires },
        })
    }
}

impl<'a> TryFrom<&'a HeaderMap> for Fields<'a> {
    type Error = Failure;

    fn try_from(headers: &'a HeaderMap) -> Result<Self, Self::Error> {
        let raw = single(headers, "authorization")?.ok_or(Failure::Malformed)?;
        let (algorithm, fields) = raw.split_once(' ').ok_or(Failure::Malformed)?;
        let algorithm: Algorithm = algorithm.parse()?;
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
        let date = single(headers, algorithm.family().date_header())?.ok_or(Failure::Malformed)?;
        Ok(Self {
            algorithm,
            credential: required("Credential")?,
            date,
            signed_headers: required("SignedHeaders")?,
            signature: required("Signature")?,
            signing: Signing::Header,
        })
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = NotV4Parameter;
    assert_eq!(error.to_string(), "not a V4 signing query parameter");
    assert!(std::error::Error::source(&error).is_none());
}
