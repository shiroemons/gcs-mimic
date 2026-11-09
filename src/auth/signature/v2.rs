//! Legacy RSA/HMAC query and GOOG1/AWS header canonicalization.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::*;

mod legacy;
pub(crate) use legacy::{has_header, legacy_aws, xml_query};

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

#[cfg(test)]
pub(super) fn prepare(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    now: SystemTime,
) -> Result<v4::Prepared, Failure> {
    prepare_resource(method, uri, headers, now, &Resource::PathStyle)
}

pub(super) fn prepare_resource(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    now: SystemTime,
    resource: &Resource,
) -> Result<v4::Prepared, Failure> {
    if has_header(headers) {
        return legacy::prepare(method, uri, headers, now, resource);
    }
    let (query, deadline) = query_fields(uri)?;
    if headers.contains_key("authorization") {
        return Err(Failure::Malformed);
    }
    if now >= deadline {
        return Err(Failure::Expired);
    }
    Ok(v4::Prepared {
        signer: required(&query, "GoogleAccessId")?.into(),
        date: String::new(),
        algorithm: v4::Algorithm::Rsa,
        scope: String::new(),
        message: string_to_sign(
            method,
            uri,
            headers,
            &query,
            Canonical {
                date: required(&query, "Expires")?,
                profile: Profile::GoogleQuery,
                resource,
            },
        )?,
        // V2 does not sign Host.
        portless: None,
        signature: decode_signature(required(&query, "Signature")?)?,
    })
}

fn required<'a>(query: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, Failure> {
    query
        .get(name)
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .ok_or(Failure::Malformed)
}

fn query_fields(uri: &Uri) -> Result<(BTreeMap<String, String>, SystemTime), Failure> {
    let query =
        json::parse_query(uri.query().unwrap_or_default()).map_err(|_| Failure::Malformed)?;
    if query
        .keys()
        .any(|key| signing_param(key.as_bytes()).is_some_and(|family| family != SigningParams::V2))
    {
        return Err(Failure::Malformed);
    }
    required(&query, "GoogleAccessId")?;
    required(&query, "Signature")?;
    let expires = required(&query, "Expires")?;
    let seconds = expires
        .parse::<u64>()
        .ok()
        .filter(|_| expires.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(Failure::Malformed)?;
    let deadline = UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or(Failure::Malformed)?;
    Ok((query, deadline))
}

/// Historical permissive V2 checks only shape; unlike V4 it rejects malformed credential fields.
pub(super) fn validate_query_shape(uri: &Uri) -> Result<(), Failure> {
    query_fields(uri).map(|_| ())
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
    let single = |name| {
        if headers.get_all(name).iter().count() > 1 {
            return Err(Failure::Malformed);
        }
        headers
            .get(name)
            .map(|value| value.to_str().map_err(|_| Failure::Malformed))
            .transpose()
            .map(|value| value.unwrap_or_default())
    };
    let mut message = format!(
        "{method}\n{}\n{}\n{date}\n",
        single("content-md5")?,
        single("content-type")?
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
