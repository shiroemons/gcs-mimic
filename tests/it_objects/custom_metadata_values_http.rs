//! Non-string custom metadata values, following what Cloud Storage recorded on 2026-11-09.

use crate::common;
use common::TestDir;
use common::effects::Server;
use serde_json::{Value, json};

const BUCKET: &str = "/storage/v1/b/test-bucket";
const OBJECT: &str = "/storage/v1/b/test-bucket/o/object";

fn setup() -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("object", b"object-bytes").json_ok();
    (dir, server)
}

/// Sends `metadata` as written, so number spellings such as `1.50` reach the server intact.
fn write(server: &Server, method: &str, metadata: &str) -> Value {
    server
        .request(
            method,
            OBJECT,
            "",
            format!(r#"{{"metadata":{metadata}}}"#).as_bytes(),
        )
        .json_ok()
}

#[test]
fn numbers_and_booleans_are_stored_as_strings() {
    let (_dir, server) = setup();
    for (literal, stored) in [
        ("1", "1"),
        ("-1", "-1"),
        ("1.5", "1.5"),
        ("1.50", "1.5"),
        ("1e3", "1000.0"),
        ("12345678901234567890", "12345678901234567890"),
        ("18446744073709551615", "18446744073709551615"),
        ("18446744073709551616", "1.844674407370955E19"),
        ("-123456789012345678901234567890", "-1.234567890123457E29"),
        ("0.0", "0.0E"),
        ("0.00", "0.0E"),
        ("0e0", "0.0E"),
        ("-0.0", "-0.0E"),
        ("0", "0"),
        ("0.001", "0.001"),
        ("0.00099", "9.9E-4"),
        ("-1e-4", "-1.0E-4"),
        ("9999999.0", "9999999.0"),
        ("1e7", "1.0E7"),
        ("12345678.5", "1.23456785E7"),
        ("1e21", "1.0E21"),
        ("1.5e300", "1.5E300"),
        ("0.1234567890123456789", "0.12345678901234568"),
        ("true", "true"),
        ("false", "false"),
    ] {
        let expected = json!({ "value": stored });
        let written = write(&server, "PATCH", &format!(r#"{{"value":{literal}}}"#));
        assert_eq!(written["metadata"], expected, "{literal}");
        assert_eq!(
            server.get("object").json_ok()["metadata"],
            expected,
            "{literal}"
        );
    }
    let removed = write(&server, "PATCH", r#"{"value":null}"#);
    assert!(removed.get("metadata").is_none());
}

#[test]
fn object_and_array_values_are_dropped_while_other_keys_apply() {
    let (_dir, server) = setup();
    write(&server, "PATCH", r#"{"keep":"1"}"#);
    let merged = write(&server, "PATCH", r#"{"new":"2","o":{"x":"1"},"a":[1]}"#);
    assert_eq!(merged["metadata"], json!({"keep":"1","new":"2"}));
    let replaced = write(&server, "PUT", r#"{"k":"v","o":{"x":"1"}}"#);
    assert_eq!(replaced["metadata"], json!({"k":"v"}));
    // The empty-key refusal does not depend on whether the value would be stored.
    for metadata in [r#"{"":1}"#, r#"{"":{"x":"1"}}"#] {
        server
            .request(
                "PATCH",
                OBJECT,
                "",
                format!(r#"{{"metadata":{metadata}}}"#).as_bytes(),
            )
            .json_reason(400, "badRequest");
    }
    assert_eq!(server.get("object").json_ok()["metadata"], json!({"k":"v"}));
}

#[test]
fn patch_clears_the_map_when_every_supplied_value_is_dropped() {
    let (_dir, server) = setup();
    for metadata in [
        r#"{"o":{"x":"1"}}"#,
        r#"{"o":{}}"#,
        r#"{"a":[]}"#,
        r#"{"o":{},"a":[1]}"#,
    ] {
        write(&server, "PATCH", r#"{"keep":"1"}"#);
        let cleared = write(&server, "PATCH", metadata);
        assert!(cleared.get("metadata").is_none(), "{metadata}");
    }
    // A key removal is not a dropped value, so it leaves the other keys alone.
    write(&server, "PATCH", r#"{"keep":"1","other":"2"}"#);
    let removed = write(&server, "PATCH", r#"{"other":null}"#);
    assert_eq!(removed["metadata"], json!({"keep":"1"}));
    // An object with no entries clears the map too, as Cloud Storage did.
    let cleared = write(&server, "PATCH", "{}");
    assert!(cleared.get("metadata").is_none());
}

#[test]
fn the_size_limit_counts_the_stored_text_of_a_converted_value() {
    let (_dir, server) = setup();
    let key = "k".repeat(8191);
    write(&server, "PATCH", &json!({ &key: 1 }).to_string());
    server
        .request(
            "PATCH",
            OBJECT,
            "",
            json!({"metadata":{ &key: 10 }}).to_string().as_bytes(),
        )
        .json_reason(400, "invalidArgument");
}

#[test]
fn every_body_that_writes_metadata_uses_the_same_value_rules() {
    let (_dir, server) = setup();
    let metadata = json!({"n":1,"b":true,"k":"v","o":{"x":"1"},"a":[1]});
    let stored = json!({"n":"1","b":"true","k":"v"});
    let body = json!({ "metadata": metadata }).to_string();

    let multipart = server.multipart(
        "boundary",
        &json!({"name":"multipart","metadata":metadata}).to_string(),
        b"media",
        "",
    );
    assert_eq!(multipart.json_ok()["metadata"], stored);

    let initiated = server.resumable("resumable", &json!({ "metadata": metadata }));
    initiated.expect(200);
    let session = initiated.headers["location"]
        .strip_prefix("http://127.0.0.1:4443")
        .unwrap()
        .to_owned();
    let completed = server.request("PUT", &session, "", b"media").json_ok();
    assert_eq!(completed["metadata"], stored);

    let copied = server
        .request(
            "POST",
            &format!("{OBJECT}/copyTo/b/test-bucket/o/copied"),
            "",
            body.as_bytes(),
        )
        .json_ok();
    assert_eq!(copied["metadata"], stored);

    let rewritten = server
        .request(
            "POST",
            &format!("{OBJECT}/rewriteTo/b/test-bucket/o/rewritten"),
            "",
            body.as_bytes(),
        )
        .json_ok();
    assert_eq!(rewritten["resource"]["metadata"], stored);

    let composed = server
        .request(
            "POST",
            &format!("{BUCKET}/o/composed/compose"),
            "",
            json!({"sourceObjects":[{"name":"object"}],"destination":{"metadata":metadata}})
                .to_string()
                .as_bytes(),
        )
        .json_ok();
    assert_eq!(composed["metadata"], stored);
}
