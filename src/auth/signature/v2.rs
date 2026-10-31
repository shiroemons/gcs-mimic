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
    query.is_some_and(|query| {
        query.split('&').any(|pair| {
            let key = super::super::lenient_decode(pair.split('=').next().unwrap_or_default());
            V2_PARAMETERS
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name.as_bytes()))
        })
    })
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
    let query =
        json::parse_query(uri.query().unwrap_or_default()).map_err(|_| Failure::Malformed)?;
    if headers.contains_key("authorization")
        || query.keys().any(|key| {
            PARAMETERS
                .iter()
                .chain(AWS_PARAMETERS)
                .any(|name| key.eq_ignore_ascii_case(name))
        })
    {
        return Err(Failure::Malformed);
    }
    let required = |name| {
        query
            .get(name)
            .filter(|value| !value.is_empty())
            .map(String::as_str)
            .ok_or(Failure::Malformed)
    };
    let expires = required("Expires")?;
    let seconds = expires
        .parse::<u64>()
        .ok()
        .filter(|_| expires.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(Failure::Malformed)?;
    if now
        >= UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .ok_or(Failure::Malformed)?
    {
        return Err(Failure::Expired);
    }
    Ok(v4::Prepared {
        signer: required("GoogleAccessId")?.into(),
        algorithm: v4::Algorithm::Rsa,
        scope: String::new(),
        message: string_to_sign(
            method,
            uri,
            headers,
            &query,
            Canonical {
                date: expires,
                profile: Profile::GoogleQuery,
                resource,
            },
        )?,
        signature: decode_signature(required("Signature")?)?,
    })
}

impl Auth {
    pub(super) fn authenticate_v2(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        clock: &dyn Clock,
    ) -> Result<Option<Caller<'_>>, Failure> {
        let query =
            json::parse_query(uri.query().unwrap_or_default()).map_err(|_| Failure::Malformed)?;
        if query.keys().any(|key| {
            PARAMETERS
                .iter()
                .chain(AWS_PARAMETERS)
                .any(|name| key.eq_ignore_ascii_case(name))
        }) {
            return Err(Failure::Malformed);
        }
        let required = |name: &str| {
            query
                .get(name)
                .filter(|value| !value.is_empty())
                .ok_or(Failure::Malformed)
        };
        let signer = required("GoogleAccessId")?;
        let expires = required("Expires")?;
        let signature = required("Signature")?;
        let seconds = expires
            .parse::<u64>()
            .ok()
            .filter(|_| expires.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or(Failure::Malformed)?;
        let deadline = UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .ok_or(Failure::Malformed)?;
        if self.registry.is_none() {
            return Ok(None);
        }
        if headers.contains_key("authorization") {
            return Err(Failure::Malformed);
        }
        let message = string_to_sign(
            method,
            uri,
            headers,
            &query,
            Canonical {
                date: expires,
                profile: Profile::GoogleQuery,
                resource: &Resource::PathStyle,
            },
        )?;
        let caller = self.verify_signer(signer, &message, &decode_signature(signature)?)?;
        if clock.now() >= deadline {
            return Err(Failure::Expired);
        }
        Ok(Some(caller))
    }
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
        message.push_str(&format!("{name}:{}\n", values.join(",")));
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
    let padding = raw.bytes().rev().take_while(|byte| *byte == b'=').count();
    if padding > 2 {
        return Err(Failure::Malformed);
    }
    let mut output = Vec::with_capacity(raw.len() / 4 * 3);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in raw[..raw.len() - padding].bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(Failure::Malformed),
        };
        bits = (bits << 6) | u32::from(digit);
        count += 6;
        if count >= 8 {
            count -= 8;
            output.push((bits >> count) as u8);
        }
    }
    if bits & ((1 << count) - 1) != 0 || output.len() > 1024 {
        return Err(Failure::Malformed);
    }
    Ok(output)
}
