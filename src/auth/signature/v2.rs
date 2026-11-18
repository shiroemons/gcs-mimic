//! Legacy RSA/HMAC query and GOOG1/AWS header canonicalization.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::auth::SigningKeys;

pub(super) mod legacy;
pub(crate) use legacy::xml_query;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    GoogleQuery,
    GoogleHeader,
    Aws,
}

impl Profile {
    fn prefix(self) -> &'static str {
        match self {
            Self::GoogleQuery | Self::GoogleHeader => "x-goog-",
            Self::Aws => "x-amz-",
        }
    }

    /// The extension prefix of the other provider, which a legacy header signature refuses.
    fn foreign_prefix(self) -> &'static str {
        match self {
            Self::GoogleQuery | Self::GoogleHeader => "x-amz-",
            Self::Aws => "x-goog-",
        }
    }

    /// The extension header that replaces `Date` in a legacy header signature.
    fn date_header(self) -> &'static str {
        match self {
            Self::GoogleQuery | Self::GoogleHeader => "x-goog-date",
            Self::Aws => "x-amz-date",
        }
    }
}

#[derive(Clone, Copy)]
struct Canonical<'a> {
    date: &'a str,
    profile: Profile,
    resource: &'a Resource,
}

pub(super) fn is_v2(query: Option<&str>) -> bool {
    decoded_query_names(query).any(|key| signing_param(&key) == Some(SigningParams::V2))
}

/// A path-style V2 query signature by a signer without a registered RSA key.
#[cfg(test)]
pub(super) fn prepare(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    now: SystemTime,
) -> Result<v4::Prepared, Failure> {
    prepare_query(
        method,
        uri,
        headers,
        now,
        &Resource::PathStyle,
        &SigningKeys::default(),
    )
}

/// A V2 query signature, which `keys` decide to verify with RSA or HMAC.
pub(super) fn prepare_query(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    now: SystemTime,
    resource: &Resource,
    keys: &SigningKeys,
) -> Result<v4::Prepared, Failure> {
    let query = parse_query(uri)?;
    let fields = QueryFields::parse(&query)?;
    if headers.contains_key("authorization") {
        return Err(Failure::Malformed);
    }
    if now >= fields.deadline {
        return Err(Failure::Expired);
    }
    Ok(v4::Prepared {
        signer: fields.access_id.into(),
        scheme: verify::v2_scheme(keys, fields.access_id),
        message: string_to_sign(
            method,
            uri,
            headers,
            &query,
            Canonical {
                date: fields.expires,
                profile: Profile::GoogleQuery,
                resource,
            },
        )?,
        // V2 does not sign Host.
        portless: None,
        signature: decode_signature(fields.signature)?,
    })
}

fn required<'a>(query: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, Failure> {
    query
        .get(name)
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .ok_or(Failure::Malformed)
}

fn parse_query(uri: &Uri) -> Result<BTreeMap<String, String>, Failure> {
    crate::percent::parse_query(uri.query().unwrap_or_default()).map_err(|_| Failure::Malformed)
}

/// The required fields of a V2 query signature.
struct QueryFields<'q> {
    access_id: &'q str,
    /// `Expires` as sent, which the string to sign repeats.
    expires: &'q str,
    signature: &'q str,
    deadline: SystemTime,
}

impl<'q> QueryFields<'q> {
    /// Refuses V4 parameters beside V2 ones, then reads the fields in the order a missing one
    /// is reported: `GoogleAccessId`, `Signature`, `Expires`.
    fn parse(query: &'q BTreeMap<String, String>) -> Result<Self, Failure> {
        if query.keys().any(|key| {
            signing_param(key.as_bytes()).is_some_and(|family| family != SigningParams::V2)
        }) {
            return Err(Failure::Malformed);
        }
        let access_id = required(query, "GoogleAccessId")?;
        let signature = required(query, "Signature")?;
        let expires = required(query, "Expires")?;
        let seconds = crate::decimal::Grammar::Digits
            .parse::<u64>(expires)
            .ok_or(Failure::Malformed)?;
        let deadline = UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .ok_or(Failure::Malformed)?;
        Ok(Self {
            access_id,
            expires,
            signature,
            deadline,
        })
    }
}

/// Historical permissive V2 checks only shape; unlike V4 it rejects malformed credential fields.
pub(super) fn validate_query_shape(uri: &Uri) -> Result<(), Failure> {
    QueryFields::parse(&parse_query(uri)?).map(drop)
}

fn string_to_sign(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    query: &BTreeMap<String, String>,
    canonical: Canonical<'_>,
) -> Result<String, Failure> {
    let Canonical {
        date,
        profile,
        resource,
    } = canonical;
    let mut message = format!(
        "{method}\n{}\n{}\n{date}\n",
        single(headers, "content-md5")?.unwrap_or_default(),
        single(headers, "content-type")?.unwrap_or_default()
    );
    let mut names: Vec<_> = headers
        .keys()
        .map(|name| name.as_str())
        .filter(|name| {
            name.starts_with(profile.prefix())
                && !(profile == Profile::GoogleQuery
                    && matches!(
                        *name,
                        "x-goog-encryption-key" | "x-goog-encryption-key-sha256"
                    ))
        })
        .collect();
    names.sort_unstable();
    for name in names {
        let values = headers
            .get_all(name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(|value| {
                        if profile == Profile::GoogleQuery {
                            value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
                        } else {
                            value.trim().to_owned()
                        }
                    })
                    .map_err(|_| Failure::Malformed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        message.push_str(name);
        message.push(':');
        message.push_str(&values.join(","));
        message.push('\n');
    }
    // The trusted endpoint selector supplies a virtual bucket without decoding the raw object path.
    if let Resource::VirtualHosted(bucket) = resource {
        message.push('/');
        message.push_str(bucket);
    }
    message.push_str(uri.path());
    let mut separator = '?';
    for (name, value) in query {
        if matches!(
            name.as_str(),
            "acl"
                | "billing"
                | "compose"
                | "cors"
                | "defaultObjectAcl"
                | "delete"
                | "encryptionConfig"
                | "lifecycle"
                | "location"
                | "logging"
                | "notification"
                | "partNumber"
                | "policy"
                | "requestPayment"
                | "storageClass"
                | "torrent"
                | "uploadId"
                | "uploads"
                | "userProject"
                | "versionId"
                | "versioning"
                | "versions"
                | "website"
                | "websiteConfig"
        ) || (profile != Profile::GoogleQuery
            && matches!(
                name.as_str(),
                "response-content-type"
                    | "response-content-language"
                    | "response-expires"
                    | "response-cache-control"
                    | "response-content-disposition"
                    | "response-content-encoding"
            ))
        {
            message.push(separator);
            separator = '&';
            message.push_str(name);
            if !value.is_empty() {
                message.push('=');
                if profile == Profile::GoogleQuery {
                    message.push_str(&encode_query_component(value));
                } else {
                    message.push_str(value);
                }
            }
        }
    }
    Ok(message)
}

pub(super) fn decode_signature(raw: &str) -> Result<Vec<u8>, Failure> {
    if raw.is_empty() || raw.len() > 1368 || !raw.len().is_multiple_of(4) {
        return Err(Failure::Malformed);
    }
    crate::base64::decode(
        raw,
        crate::base64::Alphabet::Standard,
        crate::base64::Padding::Required,
    )
    .filter(|output| output.len() <= 1024)
    .ok_or(Failure::Malformed)
}
