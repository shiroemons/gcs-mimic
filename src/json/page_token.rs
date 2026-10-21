use super::{JsonError, validate_bucket_name};
use crate::storage::{ObjectListCursor, ObjectListQuery};

const MAX_TOKEN_BYTES: usize = 4096;

/// Tokens detect accidental corruption; they are not authenticated authorization credentials.
pub fn encode_page_token(
    project: &str,
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    super::query::validate_project(project)?;
    if prefix.len() > 222 || !last_name.starts_with(prefix) {
        return Err(invalid());
    }
    validate_bucket_name(last_name)?;
    let payload = serde_json::json!(["b1", project, prefix, last_name]).to_string();
    let mut token = String::with_capacity(payload.len() * 2 + 17);
    for byte in payload.bytes() {
        use std::fmt::Write;
        write!(token, "{byte:02x}").expect("writing to String cannot fail");
    }
    use std::fmt::Write;
    write!(token, ".{:016x}", checksum(payload.as_bytes())).expect("writing to String cannot fail");
    Ok(token)
}

pub fn decode_page_token(token: &str, project: &str, prefix: &str) -> Result<String, JsonError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(invalid());
    }
    let (encoded, digest) = token.split_once('.').ok_or_else(invalid)?;
    if encoded.len() % 2 != 0 || digest.len() != 16 || !encoded.is_ascii() {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().as_chunks::<2>().0 {
        bytes.push((hex(pair[0]).ok_or_else(invalid)? << 4) | hex(pair[1]).ok_or_else(invalid)?);
    }
    if u64::from_str_radix(digest, 16).map_err(|_| invalid())? != checksum(&bytes) {
        return Err(invalid());
    }
    let fields: Vec<String> = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let [version, token_project, token_prefix, name] = fields.as_slice() else {
        return Err(invalid());
    };
    if version != "b1"
        || token_project != project
        || token_prefix != prefix
        || !name.starts_with(prefix)
    {
        return Err(invalid());
    }
    validate_bucket_name(name).map_err(|_| invalid())?;
    Ok(name.clone())
}

fn object_scope(query: &ObjectListQuery, projection: super::Projection) -> String {
    let payload = serde_json::json!([
        query.bucket,
        query.prefix,
        query.delimiter,
        query.start_offset,
        query.end_offset,
        query.include_trailing_delimiter,
        projection == super::Projection::Full
    ])
    .to_string();
    format!("{:016x}", checksum(payload.as_bytes()))
}

pub(super) fn encode_object_page_token(
    query: &ObjectListQuery,
    projection: super::Projection,
    cursor: &ObjectListCursor,
) -> String {
    let payload = serde_json::json!([
        "o1",
        object_scope(query, projection),
        cursor.name,
        cursor.is_prefix
    ])
    .to_string();
    let encoded: String = payload.bytes().map(|byte| format!("{byte:02x}")).collect();
    format!("{encoded}.{:016x}", checksum(payload.as_bytes()))
}

pub(super) fn decode_object_page_token(
    token: &str,
    query: &ObjectListQuery,
    projection: super::Projection,
) -> Result<ObjectListCursor, JsonError> {
    let invalid =
        || JsonError::invalid_parameter("Invalid pageToken for this object listing query");
    if token.len() > 16 * 1024 {
        return Err(invalid());
    }
    let (encoded, digest) = token.split_once('.').ok_or_else(invalid)?;
    if encoded.len() % 2 != 0 || digest.len() != 16 || !encoded.is_ascii() {
        return Err(invalid());
    }
    let bytes = encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(
            |pair| Ok((hex(pair[0]).ok_or_else(invalid)? << 4) | hex(pair[1]).ok_or_else(invalid)?),
        )
        .collect::<Result<Vec<_>, JsonError>>()?;
    if u64::from_str_radix(digest, 16).map_err(|_| invalid())? != checksum(&bytes) {
        return Err(invalid());
    }
    let (version, scope, name, is_prefix): (String, String, String, bool) =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if version != "o1"
        || scope != object_scope(query, projection)
        || name.is_empty()
        || name.len() > 1024
        || !name.starts_with(&query.prefix)
    {
        return Err(invalid());
    }
    if is_prefix {
        if query.delimiter.is_empty()
            || name[query.prefix.len()..]
                .find(&query.delimiter)
                .is_none_or(|index| {
                    query.prefix.len() + index + query.delimiter.len() != name.len()
                })
        {
            return Err(invalid());
        }
    } else {
        super::validate_object_name(&name).map_err(|_| invalid())?;
    }
    Ok(ObjectListCursor { name, is_prefix })
}

fn invalid() -> JsonError {
    JsonError::invalid_parameter("Invalid pageToken for this project and prefix")
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

pub(super) fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_list_tokens_reject_valid_checksums_with_invalid_payloads() {
        let query = ObjectListQuery {
            bucket: "bucket".into(),
            prefix: "a".into(),
            delimiter: "/".into(),
            ..Default::default()
        };
        let projection = super::super::Projection::NoAcl;
        let scope = object_scope(&query, projection);
        for payload in [
            serde_json::json!(["o2", scope, "a", false]),
            serde_json::json!(["o1", "wrong", "a", false]),
            serde_json::json!(["o1", scope, "b", false]),
            serde_json::json!(["o1", scope, "a/no/", true]),
            serde_json::json!(["o1", scope, "a", true]),
            serde_json::json!(["o1", scope, "", false]),
            serde_json::json!(["o1", scope, "a", 1]),
            serde_json::json!(["o1", scope]),
            serde_json::json!({}),
        ] {
            let payload = payload.to_string();
            let encoded: String = payload.bytes().map(|byte| format!("{byte:02x}")).collect();
            let token = format!("{encoded}.{:016x}", checksum(payload.as_bytes()));
            assert!(
                decode_object_page_token(&token, &query, projection).is_err(),
                "{payload}"
            );
        }
        for token in [
            "00.0",
            "gg.0000000000000000",
            "0.0000000000000000",
            &"a".repeat(16385),
        ] {
            assert!(decode_object_page_token(token, &query, projection).is_err());
        }
    }

    #[test]
    fn bkt_lst_002_rejects_valid_checksum_with_invalid_version_or_payload() {
        for payload in [
            r#"["b2","p","","abc"]"#,
            r#"["b1","p","","Bad"]"#,
            r#"["b1","p","a","bbb"]"#,
            r#"["b1","p"]"#,
            r#"{}"#,
            r#"[1,"p","","abc"]"#,
            "not JSON",
        ] {
            let encoded: String = payload.bytes().map(|byte| format!("{byte:02x}")).collect();
            let token = format!("{encoded}.{:016x}", checksum(payload.as_bytes()));
            assert!(decode_page_token(&token, "p", "").is_err(), "{payload}");
        }
    }
}
