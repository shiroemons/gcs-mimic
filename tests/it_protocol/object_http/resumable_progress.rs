//! What a resumable session reports about the bytes it has persisted, and how a chunk past the
//! session's total ends it. Both follow a Cloud Storage recording (2026-11-15, case
//! obj-resumable-declared-length), whose 256 KiB of `a` gave the digests below.
use super::resumable_open_end::{CHUNK, completed, media, pattern, put, session, status};
use super::*;
use md5::{Digest, Md5};

/// The `x-goog-running-hash` and `x-range-md5` Cloud Storage sent for 262144 bytes of `a`.
pub(super) const RECORDED_DIGESTS: (&str, &str) =
    ("crc32c=374w4A==", "c946b71bb69c07daf25470742c967e7c");

pub(super) fn digests(reply: &Reply) -> (Option<&str>, Option<&str>) {
    let header = |name| reply.headers.get(name).map(String::as_str);
    (header("x-goog-running-hash"), header("x-range-md5"))
}

/// The two headers for `bytes`, computed independently of the server.
fn expected(bytes: &[u8]) -> (String, String) {
    let crc32c = crc32c::crc32c(bytes).to_be_bytes();
    let md5: [u8; 16] = Md5::digest(bytes).into();
    (
        format!("crc32c={}", super::super::wire_parity_http::base64(&crc32c)),
        md5.iter().map(|byte| format!("{byte:02x}")).collect(),
    )
}

#[test]
fn each_308_after_data_reports_the_digests_of_the_persisted_bytes() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();
    let path = session(&server, "digests", "");
    // A session that holds no data reports neither its range nor digests.
    let empty = status(&server, &path);
    assert!(!empty.headers.contains_key("range"));
    assert_eq!(digests(&empty), (None, None));

    let chunk = put(&server, &path, "bytes 0-262143/*", &[b'a'; CHUNK]);
    chunk.expect(308);
    let recorded = (Some(RECORDED_DIGESTS.0), Some(RECORDED_DIGESTS.1));
    assert_eq!(digests(&chunk), recorded);
    assert_eq!(digests(&status(&server, &path)), recorded);

    // A chunk that resends part of the persisted prefix digests only the bytes it appends:
    // the prefix keeps its original bytes, so the digests describe those.
    let object = pattern(3 * CHUNK);
    let mut resent = vec![b'a'; CHUNK];
    resent.extend_from_slice(&object[CHUNK..2 * CHUNK]);
    put(
        &server,
        &path,
        &format!("bytes 0-{}/*", 2 * CHUNK - 1),
        &resent,
    )
    .expect(308);
    let persisted = [&[b'a'; CHUNK][..], &object[CHUNK..2 * CHUNK]].concat();
    let (crc32c, md5) = expected(&persisted);
    assert_eq!(
        digests(&status(&server, &path)),
        (Some(crc32c.as_str()), Some(md5.as_str()))
    );

    let tail = &object[2 * CHUNK..];
    let total = 3 * CHUNK;
    let resource = completed(&put(
        &server,
        &path,
        &format!("bytes {}-{}/{total}", 2 * CHUNK, total - 1),
        tail,
    ));
    let whole = [persisted.as_slice(), tail].concat();
    assert_eq!(media(&server, "digests"), whole);
    // The running digests continue into the stored checksums of the completed object.
    let (crc32c, _) = expected(&whole);
    assert_eq!(
        Some(resource["crc32c"].as_str().unwrap()),
        crc32c.strip_prefix("crc32c=")
    );
}

#[test]
fn a_chunk_past_the_declared_length_fails_the_session() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();
    let path = session(&server, "five", "X-Upload-Content-Length: 5\r\n");
    let refusal = "Invalid request.  There were 262144 byte(s) (or more) in the request body.  There should have been 5 byte(s) according to the Content-Range header.";
    put(&server, &path, "bytes 0-262143/*", &[b'a'; CHUNK]).session_error(400, refusal);
    // Every later request repeats the refusal, and no object is created.
    put(&server, &path, "bytes */*", b"").session_error(400, refusal);
    put(&server, &path, "bytes 0-4/5", b"hello").session_error(400, refusal);
    put(&server, &path, "bytes 262144-262148/262149", b"hello").session_error(400, refusal);
    server.get("five").expect(404);

    // The room counts from the chunk's start.
    let path = session(
        &server,
        "later",
        &format!("X-Upload-Content-Length: {}\r\n", CHUNK + 5),
    );
    put(&server, &path, "bytes 0-262143/*", &[b'a'; CHUNK]).expect(308);
    let refusal = "Invalid request.  There were 262144 byte(s) (or more) in the request body.  There should have been 5 byte(s) according to the Content-Range header.";
    put(&server, &path, "bytes 262144-524287/*", &[b'b'; CHUNK]).session_error(400, refusal);
    put(&server, &path, "bytes */*", b"").session_error(400, refusal);
    server.get("later").expect(404);

    // A chunk that names a conflicting total of its own is refused alone, as before.
    let path = session(&server, "named", "X-Upload-Content-Length: 5\r\n");
    put(&server, &path, "bytes 0-5/6", b"hello!").session_error(400, "Inconsistent upload total");
    completed(&put(&server, &path, "bytes 0-4/5", b"hello"));
    assert_eq!(media(&server, "named"), b"hello");
}
