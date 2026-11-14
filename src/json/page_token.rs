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
    validate_bucket_name_for_profile(last_name, CompatProfile::FakeGcsServer)?;
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

/// Soft-deleted listings continue after a (name, generation) pair; a distinct version tag
/// keeps live and soft-deleted cursors from being replayed against each other.
const SOFT_DELETED: &str = "b1s";

pub(crate) fn encode_soft_deleted_page_token(
    project: &str,
    prefix: &str,
    last_name: &str,
    last_generation: i64,
) -> Result<String, JsonError> {
    super::query::validate_project(project)?;
    if prefix.len() > 222 || !last_name.starts_with(prefix) {
        return Err(invalid());
    }
    validate_bucket_name_for_profile(last_name, CompatProfile::FakeGcsServer)?;
    let payload = serde_json::json!([
        SOFT_DELETED,
        project,
        prefix,
        last_name,
        last_generation.to_string()
    ])
    .to_string();
    let encoded = crate::hashing::encode_lower(payload.as_bytes());
    Ok(format!("{encoded}.{:016x}", checksum(payload.as_bytes())))
}

pub(crate) fn decode_soft_deleted_page_token(
    token: &str,
    project: &str,
    prefix: &str,
) -> Result<(String, i64), JsonError> {
    let fields = decode_cursor_fields(token)?;
    let [version, token_project, token_prefix, name, generation] = fields.as_slice() else {
        return Err(invalid());
    };
    if version != SOFT_DELETED
        || token_project != project
        || token_prefix != prefix
        || !name.starts_with(prefix)
    {
        return Err(invalid());
    }
    validate_bucket_name_for_profile(name, CompatProfile::FakeGcsServer).map_err(|_| invalid())?;
    let generation = generation.parse::<i64>().map_err(|_| invalid())?;
    Ok((name.clone(), generation))
}

fn decode_cursor_fields(token: &str) -> Result<Vec<String>, JsonError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(invalid());
    }
    let (encoded, digest) = token.split_once('.').ok_or_else(invalid)?;
    if encoded.len() % 2 != 0 || digest.len() != 16 || !encoded.is_ascii() {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().as_chunks::<2>().0 {
        bytes.push(crate::hex::byte(pair).ok_or_else(invalid)?);
    }
    if u64::from_str_radix(digest, 16).map_err(|_| invalid())? != checksum(&bytes) {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

fn decode_bucket_cursor(
    token: &str,
    expected_version: &str,
    project: &str,
    prefix: &str,
) -> Result<String, JsonError> {
    let fields = decode_cursor_fields(token)?;
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
    validate_bucket_name_for_profile(name, CompatProfile::FakeGcsServer).map_err(|_| invalid())?;
    Ok(name.clone())
}

/// Version tag of object listing tokens. A token holds only the position of the last entry:
/// Cloud Storage continued after that name when a token was reused with another prefix,
/// delimiter or versions flag (recorded 2026-11), so the listing parameters are not bound in.
const OBJECT_CURSOR: &str = "o3";

pub(crate) fn encode_object_page_token(cursor: &ObjectListCursor) -> String {
    let payload = serde_json::json!([
        OBJECT_CURSOR,
        cursor.name,
        cursor.is_prefix,
        cursor.generation
    ])
    .to_string();
    let encoded = crate::hashing::encode_lower(payload.as_bytes());
    format!("{encoded}.{:016x}", checksum(payload.as_bytes()))
}

/// The longest refused object page token an error repeats.
const NAMED_TOKEN_BYTES: usize = 256;

/// The position a token names, read for `query`. A prefix entry continues past its whole group
/// only when `query` groups by the same delimiter; otherwise the prefix is a plain name to
/// continue after. A generation is kept only for listings that page through generations.
pub(crate) fn decode_object_page_token(
    token: &str,
    query: &ObjectListQuery,
) -> Result<ObjectListCursor, JsonError> {
    // Cloud Storage names the token it refuses. Only a short one is named here, so the size of an
    // error stays independent of what the client sent.
    let invalid = || {
        if token.len() > NAMED_TOKEN_BYTES {
            JsonError::invalid("Next page token not valid")
        } else {
            JsonError::invalid(format!("Next page token not valid: {token}"))
        }
    };
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
        .map(|pair| crate::hex::byte(pair).ok_or_else(invalid))
        .collect::<Result<Vec<_>, JsonError>>()?;
    if u64::from_str_radix(digest, 16).map_err(|_| invalid())? != checksum(&bytes) {
        return Err(invalid());
    }
    let (version, name, is_prefix, generation): (String, String, bool, Option<i64>) =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if version != OBJECT_CURSOR
        || name.is_empty()
        || name.len() > 1024
        || generation.is_some_and(|value| value <= 0)
        || (is_prefix && generation.is_some())
    {
        return Err(invalid());
    }
    if !is_prefix {
        super::validate_object_name(&name).map_err(|_| invalid())?;
    }
    let is_prefix = is_prefix && groups(query, &name);
    let paged_generations = (query.versions || query.soft_deleted) && !is_prefix;
    Ok(ObjectListCursor {
        name,
        is_prefix,
        generation: generation.filter(|_| paged_generations),
    })
}

/// Whether `query` lists `name` as a prefix entry: the name is the query prefix followed by a
/// segment that ends at the first delimiter.
fn groups(query: &ObjectListQuery, name: &str) -> bool {
    !query.delimiter.is_empty()
        && name.strip_prefix(&query.prefix).is_some_and(|rest| {
            rest.find(&query.delimiter)
                .is_some_and(|index| index + query.delimiter.len() == rest.len())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(token: &str) -> String {
        decode_object_page_token(token, &ObjectListQuery::default())
            .unwrap_err()
            .message
    }

    #[test]
    fn invalid_object_tokens_do_not_echo_untrusted_input() {
        assert_eq!(
            refusal(&"sensitive-token".repeat(1200)),
            "Next page token not valid"
        );
    }

    #[test]
    fn a_refused_object_token_is_named_up_to_the_bound() {
        assert_eq!(refusal("bogus"), "Next page token not valid: bogus");
        let longest = "x".repeat(NAMED_TOKEN_BYTES);
        assert_eq!(
            refusal(&longest),
            format!("Next page token not valid: {longest}")
        );
        assert_eq!(
            refusal(&"x".repeat(NAMED_TOKEN_BYTES + 1)),
            "Next page token not valid"
        );
    }

    fn forged_token(payload: &str) -> String {
        let encoded = crate::hashing::encode_lower(payload.as_bytes());
        format!("{encoded}.{:016x}", checksum(payload.as_bytes()))
    }

    #[test]
    fn object_list_tokens_reject_valid_checksums_with_invalid_payloads() {
        let query = ObjectListQuery {
            bucket: "bucket".into(),
            prefix: "a".into(),
            delimiter: "/".into(),
            ..Default::default()
        };
        for payload in [
            serde_json::json!(["o2", "a", false, null]),
            serde_json::json!(["o3", "", false, null]),
            serde_json::json!(["o3", "a".repeat(1025), false, null]),
            serde_json::json!(["o3", "..", false, null]),
            serde_json::json!(["o3", "a", 1, null]),
            serde_json::json!(["o3", "a", false, 0]),
            serde_json::json!(["o3", "a", false, -1]),
            serde_json::json!(["o3", "a/", true, 1]),
            serde_json::json!(["o3", "a"]),
            serde_json::json!({}),
        ] {
            let payload = payload.to_string();
            let token = forged_token(&payload);
            assert!(
                decode_object_page_token(&token, &query).is_err(),
                "{payload}"
            );
        }
        for token in [
            "00.0",
            "gg.0000000000000000",
            "0.0000000000000000",
            &"a".repeat(16385),
        ] {
            assert!(decode_object_page_token(token, &query).is_err(), "{token}");
        }
        let versioned = ObjectListQuery {
            versions: true,
            ..query
        };
        let cursor = ObjectListCursor {
            name: "a".into(),
            is_prefix: false,
            generation: Some(i64::MAX),
        };
        let token = encode_object_page_token(&cursor);
        assert_eq!(
            decode_object_page_token(&token, &versioned).unwrap(),
            cursor
        );
    }

    #[test]
    fn object_list_tokens_are_positions_that_any_listing_can_continue_from() {
        let cursor = |name: &str, is_prefix, generation| ObjectListCursor {
            name: name.into(),
            is_prefix,
            generation,
        };
        let query = |prefix: &str, delimiter: &str, versions| ObjectListQuery {
            bucket: "bucket".into(),
            prefix: prefix.into(),
            delimiter: delimiter.into(),
            versions,
            ..Default::default()
        };
        for (token, query, expected) in [
            // Another prefix, even one the name is outside of.
            (
                cursor("a/x", false, None),
                query("b/", "", false),
                cursor("a/x", false, None),
            ),
            // A generation is dropped where generations are not paged, and absent from a
            // versioned listing that continues after a plain name.
            (
                cursor("a/x", false, Some(3)),
                query("a/", "", false),
                cursor("a/x", false, None),
            ),
            (
                cursor("a/x", false, None),
                query("a/", "", true),
                cursor("a/x", false, None),
            ),
            // A prefix keeps skipping its group only under the delimiter that formed it.
            (
                cursor("a/b-", true, None),
                query("a/", "-", false),
                cursor("a/b-", true, None),
            ),
            (
                cursor("a/b-", true, None),
                query("a/", "", true),
                cursor("a/b-", false, None),
            ),
            (
                cursor("a/b-", true, None),
                query("a/", "/", false),
                cursor("a/b-", false, None),
            ),
        ] {
            let decoded =
                decode_object_page_token(&encode_object_page_token(&token), &query).unwrap();
            assert_eq!(decoded, expected, "{token:?} {query:?}");
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
            let token = forged_token(payload);
            assert!(decode_page_token(&token, "p", "").is_err(), "{payload}");
        }
    }

    /// Clients keep page tokens across requests, and the gRPC and XML adapters hand out the
    /// same text, so a token must keep its bytes and a token issued earlier must still open.
    #[test]
    fn page_tokens_keep_their_bytes_and_open() {
        const BUCKET: &str = "5b226231222c2270726f6a6563742d31222c226c6f6773222c226c6f67732d32303234225d.2a695ec2890f728e";
        assert_eq!(
            encode_page_token("project-1", "logs", "logs-2024").unwrap(),
            BUCKET
        );
        assert_eq!(
            decode_page_token(BUCKET, "project-1", "logs").unwrap(),
            "logs-2024"
        );
        const ALL_PROJECTS: &str =
            "5b2262312a222c22222c22222c226275636b65742d61225d.132d4f68d056d91e";
        assert_eq!(
            encode_all_projects_page_token("", "bucket-a").unwrap(),
            ALL_PROJECTS
        );
        assert_eq!(
            decode_all_projects_page_token(ALL_PROJECTS, "").unwrap(),
            "bucket-a"
        );
        const SOFT_DELETED: &str = "5b22623173222c2270726f6a6563742d31222c22222c2264656c657465642d6275636b6574222c2231373030303030303030313233225d.60f83ca2962239f6";
        assert_eq!(
            encode_soft_deleted_page_token("project-1", "", "deleted-bucket", 1_700_000_000_123)
                .unwrap(),
            SOFT_DELETED
        );
        assert_eq!(
            decode_soft_deleted_page_token(SOFT_DELETED, "project-1", "").unwrap(),
            ("deleted-bucket".to_owned(), 1_700_000_000_123)
        );
        for (token, cursor, query) in [
            (
                "5b226f33222c22615c22625c5c632fe697a5e69cac5c7530303031222c66616c73652c6e756c6c5d.1357486b6935cd9a",
                ObjectListCursor {
                    name: "a\"b\\c/日本\u{1}".into(),
                    is_prefix: false,
                    generation: None,
                },
                ObjectListQuery::default(),
            ),
            (
                "5b226f33222c226c6f67732f222c747275652c6e756c6c5d.3819579151678fd0",
                ObjectListCursor {
                    name: "logs/".into(),
                    is_prefix: true,
                    generation: None,
                },
                ObjectListQuery {
                    delimiter: "/".into(),
                    ..Default::default()
                },
            ),
            (
                "5b226f33222c2276222c66616c73652c393232333337323033363835343737353830375d.37c294a36584de78",
                ObjectListCursor {
                    name: "v".into(),
                    is_prefix: false,
                    generation: Some(i64::MAX),
                },
                ObjectListQuery {
                    versions: true,
                    ..Default::default()
                },
            ),
        ] {
            assert_eq!(encode_object_page_token(&cursor), token, "{cursor:?}");
            assert_eq!(decode_object_page_token(token, &query).unwrap(), cursor);
        }
    }
}
