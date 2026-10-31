use super::{JsonError, validate_bucket_name_for_profile};
use crate::config::CompatProfile;
use crate::storage::{ObjectListCursor, ObjectListQuery};

const MAX_TOKEN_BYTES: usize = 4096;

/// Version tag of cursors that span every project. Only the legacy gRPC adapter lists without a
/// project; a distinct tag keeps those cursors from being replayed as one project's cursor.
const ALL_PROJECTS: &str = "b1*";

/// Tokens detect accidental corruption; they are not authenticated authorization credentials.
pub(crate) fn encode_page_token(
    project: &str,
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    super::query::validate_project(project)?;
    encode_bucket_cursor("b1", project, prefix, last_name)
}

/// Encodes a cursor for a listing that spans every project the caller may list.
pub(crate) fn encode_all_projects_page_token(
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    encode_bucket_cursor(ALL_PROJECTS, "", prefix, last_name)
}

fn encode_bucket_cursor(
    version: &str,
    project: &str,
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    if prefix.len() > 222 || !last_name.starts_with(prefix) {
        return Err(invalid());
    }
    // Native listing must retain cursors for source-only names after a profile switch.
    validate_bucket_name_for_profile(last_name, CompatProfile::LegacyEmulator)?;
    let payload = serde_json::json!([version, project, prefix, last_name]).to_string();
    let encoded = crate::hashing::encode_lower(payload.as_bytes());
    Ok(format!("{encoded}.{:016x}", checksum(payload.as_bytes())))
}

pub(crate) fn decode_page_token(
    token: &str,
    project: &str,
    prefix: &str,
) -> Result<String, JsonError> {
    decode_bucket_cursor(token, "b1", project, prefix)
}

pub(crate) fn decode_all_projects_page_token(
    token: &str,
    prefix: &str,
) -> Result<String, JsonError> {
    decode_bucket_cursor(token, ALL_PROJECTS, "", prefix)
}

fn decode_bucket_cursor(
    token: &str,
    expected_version: &str,
    project: &str,
    prefix: &str,
) -> Result<String, JsonError> {
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
    if version != expected_version
        || token_project != project
        || token_prefix != prefix
        || !name.starts_with(prefix)
    {
        return Err(invalid());
    }
    validate_bucket_name_for_profile(name, CompatProfile::LegacyEmulator).map_err(|_| invalid())?;
    Ok(name.clone())
}

fn object_scope(query: &ObjectListQuery, projection: super::Projection) -> String {
    let mut payload = vec![
        serde_json::json!(query.bucket),
        serde_json::json!(query.prefix),
        serde_json::json!(query.delimiter),
        serde_json::json!(query.start_offset),
        serde_json::json!(query.end_offset),
        serde_json::json!(query.include_trailing_delimiter),
        serde_json::json!(projection == super::Projection::Full),
        serde_json::json!(query.versions),
        serde_json::json!(query.soft_deleted),
    ];
    if let Some(glob) = &query.match_glob {
        payload.push(glob.pattern().into());
    }
    let payload = serde_json::Value::Array(payload).to_string();
    format!("{:016x}", checksum(payload.as_bytes()))
}

pub(crate) fn encode_object_page_token(
    query: &ObjectListQuery,
    projection: super::Projection,
    cursor: &ObjectListCursor,
) -> String {
    let payload = serde_json::json!([
        "o2",
        object_scope(query, projection),
        cursor.name,
        cursor.is_prefix,
        cursor.generation
    ])
    .to_string();
    let encoded = crate::hashing::encode_lower(payload.as_bytes());
    format!("{encoded}.{:016x}", checksum(payload.as_bytes()))
}

pub(crate) fn decode_object_page_token(
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
    let (version, scope, name, is_prefix, generation): (String, String, String, bool, Option<i64>) =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if version != "o2"
        || scope != object_scope(query, projection)
        || name.is_empty()
        || name.len() > 1024
        || !name.starts_with(&query.prefix)
        || generation.is_some_and(|value| value <= 0)
        || generation.is_some() != ((query.versions || query.soft_deleted) && !is_prefix)
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
    Ok(ObjectListCursor {
        name,
        is_prefix,
        generation,
    })
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
            serde_json::json!(["o1", scope, "a", false, null]),
            serde_json::json!(["o2", "wrong", "a", false, null]),
            serde_json::json!(["o2", scope, "b", false, null]),
            serde_json::json!(["o2", scope, "a/no/", true, null]),
            serde_json::json!(["o2", scope, "a", true, null]),
            serde_json::json!(["o2", scope, "", false, null]),
            serde_json::json!(["o2", scope, "a", 1, null]),
            serde_json::json!(["o2", scope, "a", false, 1]),
            serde_json::json!(["o2", scope]),
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
        let query = ObjectListQuery {
            versions: true,
            ..query
        };
        let scope = object_scope(&query, projection);
        for (name, is_prefix, generation) in [
            ("a", false, None),
            ("a", false, Some(0)),
            ("a", false, Some(-1)),
            ("a/", true, Some(1)),
        ] {
            let cursor = ObjectListCursor {
                name: name.into(),
                is_prefix,
                generation,
            };
            let payload = serde_json::json!([
                "o2",
                scope,
                cursor.name,
                cursor.is_prefix,
                cursor.generation
            ])
            .to_string();
            let encoded: String = payload.bytes().map(|byte| format!("{byte:02x}")).collect();
            let token = format!("{encoded}.{:016x}", checksum(payload.as_bytes()));
            assert!(
                decode_object_page_token(&token, &query, projection).is_err(),
                "{payload}"
            );
        }
        let cursor = ObjectListCursor {
            name: "a".into(),
            is_prefix: false,
            generation: Some(i64::MAX),
        };
        let token = encode_object_page_token(&query, projection, &cursor);
        assert_eq!(
            decode_object_page_token(&token, &query, projection).unwrap(),
            cursor
        );
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
