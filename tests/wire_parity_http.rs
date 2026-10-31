//! Byte-level response parity with responses recorded from Cloud Storage: entity tags,
//! cache headers, error bodies, JSON formatting and XML documents.

mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Reply, Server, gzip};
use serde_json::Value;

const JSON: &str = "Content-Type: application/json\r\n";
const OBJECT: &str = "/storage/v1/b/parity/o/plain.txt";

fn setup(server: &Server) -> Value {
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            JSON,
            br#"{"name":"parity"}"#,
        )
        .status(200);
    let uploaded = server.request(
        "POST",
        "/upload/storage/v1/b/parity/o?uploadType=media&name=plain.txt",
        "Content-Type: text/plain\r\n",
        b"plain text body\n",
    );
    uploaded.status(200);
    uploaded.json()
}

/// Every value of one response header, in order.
fn header_values<'a>(reply: &'a Reply, name: &str) -> Vec<&'a str> {
    reply
        .headers
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .collect()
}

fn header<'a>(reply: &'a Reply, name: &str) -> Option<&'a str> {
    let values = header_values(reply, name);
    assert!(values.len() <= 1, "{name} repeated: {values:?}");
    values.first().copied()
}

fn varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        bytes.push((value as u8) | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(word >> (18 - 6 * index) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The protobuf message Cloud Storage encodes into JSON ETags: field 1 is the object
/// generation (absent for buckets) and field 2 the metageneration.
fn expected_etag(generation: Option<u64>, metageneration: u64) -> String {
    let mut bytes = Vec::new();
    if let Some(generation) = generation {
        bytes.push(0x08);
        varint(&mut bytes, generation);
        bytes.push(0x10);
    } else {
        bytes.push(0x08);
    }
    varint(&mut bytes, metageneration);
    base64(&bytes)
}

#[test]
fn json_etags_encode_generation_and_metageneration_like_cloud_storage() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let uploaded = setup(&server);
    let generation: u64 = uploaded["generation"].as_str().unwrap().parse().unwrap();
    let etag = expected_etag(Some(generation), 1);
    assert_eq!(uploaded["etag"], etag);

    let metadata = server.request("GET", OBJECT, "", b"");
    metadata.status(200);
    assert_eq!(header(&metadata, "etag"), Some(etag.as_str()));
    assert_eq!(metadata.json()["etag"], etag);
    let media = server.request("GET", &format!("{OBJECT}?alt=media"), "", b"");
    media.status(200);
    assert_eq!(header(&media, "etag"), Some(etag.as_str()));
    // The XML API keeps the quoted MD5 validator.
    let xml = server.request("GET", "/parity/plain.txt", "", b"");
    xml.status(200);
    assert_eq!(
        header(&xml, "etag"),
        Some("\"dd2e843409823500b8dbd87f539398e5\"")
    );

    // SDKs copy the unquoted body etag into If-Match and If-None-Match verbatim; the quoted
    // form names the same tag.
    for value in [etag.clone(), format!("\"{etag}\"")] {
        let cached = server.request("GET", OBJECT, &format!("If-None-Match: {value}\r\n"), b"");
        assert_eq!(cached.status, 304, "{value}");
    }
    let patched = server.request(
        "PATCH",
        OBJECT,
        &format!("{JSON}If-Match: {etag}\r\n"),
        br#"{"metadata":{"k":"v"}}"#,
    );
    patched.status(200);
    let patched_etag = expected_etag(Some(generation), 2);
    assert_eq!(patched.json()["etag"], patched_etag);
    assert_eq!(header(&patched, "etag"), None);
    // The .NET client sends the fetched resource gzip-compressed with its etag as If-Match.
    let mut resource = patched.json();
    resource["metadata"] = serde_json::json!({"k":"replaced"});
    let replaced = server.request(
        "PUT",
        OBJECT,
        &format!("{JSON}Content-Encoding: gzip\r\nIf-Match: {patched_etag}\r\n"),
        &gzip(resource.to_string().as_bytes()),
    );
    replaced.status(200);
    assert_eq!(replaced.json()["etag"], expected_etag(Some(generation), 3));
    let stale = server.request(
        "PATCH",
        OBJECT,
        &format!("{JSON}If-Match: {etag}\r\n"),
        br#"{"metadata":{"k":"lost"}}"#,
    );
    stale.status(412);
    // A bare tag that is not a valid token is still rejected: whitespace inside a member and a
    // stray quote inside a bare member stay 400, for both validators.
    for value in [
        format!("{patched_etag} extra"),
        "two tokens".to_owned(),
        "bare\"tag".to_owned(),
        format!("{patched_etag}, bad tag"),
        format!("*, {patched_etag}"),
    ] {
        for field in ["If-None-Match", "If-Match"] {
            let invalid = server.request("GET", OBJECT, &format!("{field}: {value}\r\n"), b"");
            assert_eq!(invalid.status, 400, "{field}: {value}");
        }
    }

    let bucket = server.request("GET", "/storage/v1/b/parity", "", b"");
    bucket.status(200);
    assert_eq!(header(&bucket, "etag"), Some("CAE="));
    assert_eq!(bucket.json()["etag"], "CAE=");
    let cached = server.request(
        "GET",
        "/storage/v1/b/parity",
        "If-None-Match: CAE=\r\n",
        b"",
    );
    assert_eq!(cached.status, 304);
    let relabeled = server.request(
        "PATCH",
        "/storage/v1/b/parity",
        JSON,
        br#"{"labels":{"k":"v"}}"#,
    );
    relabeled.status(200);
    assert_eq!(relabeled.json()["etag"], expected_etag(None, 2));
}
