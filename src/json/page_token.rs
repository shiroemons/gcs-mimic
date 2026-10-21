use super::{JsonError, validate_bucket_name};

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
