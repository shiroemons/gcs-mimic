//! Google documents the legacy identifiers; gsutil's pinned provider selects these rules.

use super::*;

#[cfg(test)]
mod tests;

pub(crate) fn has_header(headers: &HeaderMap) -> bool {
    headers.get_all("authorization").iter().any(|value| {
        let raw = value.as_bytes();
        raw.starts_with(b"GOOG1") || raw == b"AWS" || raw.starts_with(b"AWS ")
    })
}

pub(crate) fn legacy_aws(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .is_some_and(|value| value.as_bytes().starts_with(b"AWS "))
}

fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, Failure> {
    if headers.get_all(name).iter().count() > 1 {
        return Err(Failure::Malformed);
    }
    headers
        .get(name)
        .map(|value| value.to_str().map_err(|_| Failure::Malformed))
        .transpose()
}

pub(crate) fn xml_query(
    raw: &str,
    headers: &HeaderMap,
) -> Result<BTreeMap<String, String>, json::JsonError> {
    if !has_header(headers) {
        return json::parse_query(raw);
    }
    if raw.len() > 16 * 1024 {
        return Err(json::JsonError::invalid_parameter("Query exceeds 16 KiB"));
    }
    let mut query = BTreeMap::new();
    // Boto's legacy unquote preserves literal plus signs, unlike form decoding.
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = json::decode(key, json::Decoding::Path)?;
        let value = json::decode(value, json::Decoding::Path)?;
        if query.insert(key.clone(), value).is_some() {
            return Err(json::JsonError::invalid_parameter(format!(
                "Duplicate query parameter: {key}"
            )));
        }
    }
    Ok(query)
}

pub(super) fn prepare(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    now: SystemTime,
    resource: &Resource,
) -> Result<v4::Prepared, Failure> {
    let query =
        xml_query(uri.query().unwrap_or_default(), headers).map_err(|_| Failure::Malformed)?;
    // Generation is not part of the legacy canonical subresource and must not retarget a grant.
    if is_signed(uri.query()) || query.contains_key("generation") {
        return Err(Failure::Malformed);
    }
    let raw = single(headers, "authorization")?.ok_or(Failure::Malformed)?;
    let (kind, credential) = raw.split_once(' ').ok_or(Failure::Malformed)?;
    let profile = match kind {
        "GOOG1" => Profile::GoogleHeader,
        "AWS" => Profile::Aws,
        _ => return Err(Failure::Malformed),
    };
    let (signer, signature) = credential.split_once(':').ok_or(Failure::Malformed)?;
    if signer.is_empty()
        || signer.len() > 1024
        || signer
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || headers.keys().any(|name| {
            name.as_str()
                .starts_with(if profile == Profile::GoogleHeader {
                    "x-amz-"
                } else {
                    "x-goog-"
                })
        })
    {
        return Err(Failure::Malformed);
    }
    let date = single(headers, "date")?;
    let alternate = single(
        headers,
        if profile == Profile::GoogleHeader {
            "x-goog-date"
        } else {
            "x-amz-date"
        },
    )?;
    let effective = alternate.or(date).ok_or(Failure::Malformed)?;
    // AWS's published legacy vector uses +0000; retain its original bytes in the signature.
    let normalized = effective
        .strip_suffix("+0000")
        .map(|prefix| format!("{prefix}GMT"));
    let active = httpdate::parse_http_date(normalized.as_deref().unwrap_or(effective))
        .map_err(|_| Failure::Malformed)?;
    if active
        .duration_since(now)
        .is_ok_and(|offset| offset > Duration::from_secs(900))
    {
        return Err(Failure::NotYetValid);
    }
    if now
        .duration_since(active)
        .is_ok_and(|offset| offset > Duration::from_secs(900))
    {
        return Err(Failure::Expired);
    }
    Ok(v4::Prepared {
        signer: signer.into(),
        algorithm: v4::Algorithm::LegacyHmacSha1,
        scope: String::new(),
        message: string_to_sign(
            method,
            uri,
            headers,
            &query,
            Canonical {
                date: if alternate.is_some() { "" } else { effective },
                profile,
                resource,
            },
        )?,
        signature: decode_signature(signature)?,
    })
}
