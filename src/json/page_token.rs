use super::{JsonError, validate_bucket_name_for_profile};
use crate::config::CompatProfile;
use crate::storage::{ObjectListCursor, ObjectListQuery};
use crate::{bucket::name::MAX_BUCKET_NAME_BYTES, object::name::MAX_OBJECT_NAME_BYTES};
use serde::{Serialize, de::DeserializeOwned};

/// The tag that opens a cursor payload. Each listing has its own, so a cursor cannot be
/// replayed against another listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CursorTag {
    Buckets,
    /// Buckets of every project: only the legacy gRPC adapter lists without a project.
    AllProjectBuckets,
    /// Soft-deleted buckets, which continue after a (name, generation) pair.
    SoftDeletedBuckets,
    /// Objects. A token holds only the position of the last entry: Cloud Storage continued
    /// after that name when a token was reused with another prefix, delimiter or versions flag
    /// (recorded 2026-11), so the listing parameters are not bound in.
    Objects,
    HmacKeys,
}

impl CursorTag {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Buckets => "b1",
            Self::AllProjectBuckets => "b1*",
            Self::SoftDeletedBuckets => "b1s",
            Self::Objects => "o3",
            Self::HmacKeys => "h1",
        }
    }
}

const BUCKET_TOKEN_MAX_BYTES: usize = 4096;
pub(super) const HMAC_TOKEN_MAX_BYTES: usize = 4096;
/// Object names reach 1024 bytes and JSON escaping can grow them sixfold before hex doubles
/// them.
const OBJECT_TOKEN_MAX_BYTES: usize = 16 * 1024;

/// A token's wire form: the lowercase hex of the payload's compact JSON, `.`, and the 16
/// lowercase hex digits of the FNV-1a 64 checksum of the same bytes. The checksum detects
/// corruption; a token is not a credential. A payload is a tuple of strings, booleans and
/// integers, never a map.
#[expect(
    clippy::expect_used,
    reason = "every payload is a tuple of strings, booleans and integers, which serde_json writes to a Vec without error"
)]
pub(super) fn seal(payload: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(payload).expect("cursor payloads serialize");
    format!(
        "{}.{:016x}",
        crate::hashing::encode_lower(&bytes),
        fnv1a(&bytes)
    )
}

/// The payload of a token whose form and checksum are intact. `max_bytes` bounds the work an
/// untrusted token can cause.
pub(super) fn open<T: DeserializeOwned>(token: &str, max_bytes: usize) -> Option<T> {
    if token.len() > max_bytes {
        return None;
    }
    let (encoded, digest) = token.split_once('.')?;
    if encoded.len() % 2 != 0 || digest.len() != 16 || !encoded.is_ascii() {
        return None;
    }
    let bytes = encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(crate::hex::byte)
        .collect::<Option<Vec<u8>>>()?;
    // `from_str_radix` also takes uppercase digits and a leading `+`, as tokens always have.
    if u64::from_str_radix(digest, 16).ok()? != fnv1a(&bytes) {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

/// The project a bucket or HMAC key cursor binds: its number, the one name a project's ID and
/// number share, so a listing continues whichever of the two the next request spells.
pub(super) fn cursor_project(project: &str) -> String {
    crate::project::ProjectNumber::of(project).to_string()
}

/// Whether a cursor binds `project`: by number, or by the spelling cursors bound before they
/// named projects by number, so a token issued then still opens for that spelling.
pub(super) fn cursor_binds(bound: &str, project: &str) -> bool {
    bound == project || bound == cursor_project(project)
}

/// Tokens detect accidental corruption; they are not authenticated authorization credentials.
pub(crate) fn encode_page_token(
    project: &str,
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    super::query::validate_project(project)?;
    encode_bucket_cursor(
        CursorTag::Buckets,
        &cursor_project(project),
        prefix,
        last_name,
    )
}

/// Encodes a cursor for a listing that spans every project the caller may list.
pub(crate) fn encode_all_projects_page_token(
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    encode_bucket_cursor(CursorTag::AllProjectBuckets, "", prefix, last_name)
}

fn encode_bucket_cursor(
    tag: CursorTag,
    project: &str,
    prefix: &str,
    last_name: &str,
) -> Result<String, JsonError> {
    if prefix.len() > MAX_BUCKET_NAME_BYTES || !last_name.starts_with(prefix) {
        return Err(invalid());
    }
    // Native listing must retain cursors for source-only names after a profile switch.
    validate_bucket_name_for_profile(last_name, CompatProfile::FakeGcsServer)?;
    Ok(seal(&(tag.as_str(), project, prefix, last_name)))
}

pub(crate) fn decode_page_token(
    token: &str,
    project: &str,
    prefix: &str,
) -> Result<String, JsonError> {
    decode_bucket_cursor(token, CursorTag::Buckets, project, prefix)
}

pub(crate) fn decode_all_projects_page_token(
    token: &str,
    prefix: &str,
) -> Result<String, JsonError> {
    decode_bucket_cursor(token, CursorTag::AllProjectBuckets, "", prefix)
}

pub(crate) fn encode_soft_deleted_page_token(
    project: &str,
    prefix: &str,
    last_name: &str,
    last_generation: i64,
) -> Result<String, JsonError> {
    super::query::validate_project(project)?;
    if prefix.len() > MAX_BUCKET_NAME_BYTES || !last_name.starts_with(prefix) {
        return Err(invalid());
    }
    validate_bucket_name_for_profile(last_name, CompatProfile::FakeGcsServer)?;
    Ok(seal(&(
        CursorTag::SoftDeletedBuckets.as_str(),
        cursor_project(project),
        prefix,
        last_name,
        last_generation.to_string(),
    )))
}

pub(crate) fn decode_soft_deleted_page_token(
    token: &str,
    project: &str,
    prefix: &str,
) -> Result<(String, i64), JsonError> {
    let (tag, token_project, token_prefix, name, generation): (
        String,
        String,
        String,
        String,
        String,
    ) = open(token, BUCKET_TOKEN_MAX_BYTES).ok_or_else(invalid)?;
    if tag != CursorTag::SoftDeletedBuckets.as_str()
        || !cursor_binds(&token_project, project)
        || token_prefix != prefix
        || !name.starts_with(prefix)
    {
        return Err(invalid());
    }
    validate_bucket_name_for_profile(&name, CompatProfile::FakeGcsServer).map_err(|_| invalid())?;
    let generation = generation.parse::<i64>().map_err(|_| invalid())?;
    Ok((name, generation))
}

fn decode_bucket_cursor(
    token: &str,
    expected: CursorTag,
    project: &str,
    prefix: &str,
) -> Result<String, JsonError> {
    let (tag, token_project, token_prefix, name): (String, String, String, String) =
        open(token, BUCKET_TOKEN_MAX_BYTES).ok_or_else(invalid)?;
    if tag != expected.as_str()
        || !cursor_binds(&token_project, project)
        || token_prefix != prefix
        || !name.starts_with(prefix)
    {
        return Err(invalid());
    }
    validate_bucket_name_for_profile(&name, CompatProfile::FakeGcsServer).map_err(|_| invalid())?;
    Ok(name)
}

pub(crate) fn encode_object_page_token(cursor: &ObjectListCursor) -> String {
    seal(&(
        CursorTag::Objects.as_str(),
        &cursor.name,
        cursor.is_prefix,
        cursor.generation,
    ))
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
    let (tag, name, is_prefix, generation): (String, String, bool, Option<i64>) =
        open(token, OBJECT_TOKEN_MAX_BYTES).ok_or_else(invalid)?;
    if tag != CursorTag::Objects.as_str()
        || name.is_empty()
        || name.len() > MAX_OBJECT_NAME_BYTES
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
        format!("{encoded}.{:016x}", fnv1a(payload.as_bytes()))
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
        // A cursor binds the number of `project-1`, so it opens for the ID and for the number.
        const NUMBER: &str = "641772831285";
        const BUCKET: &str = "5b226231222c22363431373732383331323835222c226c6f6773222c226c6f67732d32303234225d.5eb3e04d7859726b";
        // Issued before cursors bound the number; it still opens for the spelling it bound.
        const EARLIER_BUCKET: &str = "5b226231222c2270726f6a6563742d31222c226c6f6773222c226c6f67732d32303234225d.2a695ec2890f728e";
        assert_eq!(
            encode_page_token("project-1", "logs", "logs-2024").unwrap(),
            BUCKET
        );
        for (token, project) in [
            (BUCKET, "project-1"),
            (BUCKET, NUMBER),
            (EARLIER_BUCKET, "project-1"),
        ] {
            assert_eq!(
                decode_page_token(token, project, "logs").unwrap(),
                "logs-2024",
                "{token} {project}"
            );
        }
        assert!(decode_page_token(BUCKET, "project-2", "logs").is_err());
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
        const SOFT_DELETED: &str = "5b22623173222c22363431373732383331323835222c22222c2264656c657465642d6275636b6574222c2231373030303030303030313233225d.606f2d128d10b853";
        const EARLIER_SOFT_DELETED: &str = "5b22623173222c2270726f6a6563742d31222c22222c2264656c657465642d6275636b6574222c2231373030303030303030313233225d.60f83ca2962239f6";
        assert_eq!(
            encode_soft_deleted_page_token("project-1", "", "deleted-bucket", 1_700_000_000_123)
                .unwrap(),
            SOFT_DELETED
        );
        for (token, project) in [
            (SOFT_DELETED, "project-1"),
            (SOFT_DELETED, NUMBER),
            (EARLIER_SOFT_DELETED, "project-1"),
        ] {
            assert_eq!(
                decode_soft_deleted_page_token(token, project, "").unwrap(),
                ("deleted-bucket".to_owned(), 1_700_000_000_123),
                "{token} {project}"
            );
        }
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
