use super::*;

const OBJECT: &str = "/iam-bucket/object";
const METADATA: &str = "/storage/v1/b/iam-bucket/o/object";
const QUERY: &str = "response-content-type=text%2Fcsv&response-content-disposition=attachment%3B%20filename%3D%22a%20%2B%20b.csv%22";

#[test]
fn xml_response_overrides_preserve_selected_metadata_ranges_and_cors() {
    let (_config, _data, server) = fixture();
    server.request("PATCH", METADATA, ADMIN, br#"{"contentType":"text/plain","contentDisposition":"inline","cacheControl":"private, max-age=42","contentLanguage":"en","metadata":{"kept":"yes"}}"#).ok();
    server.request("PATCH", "/storage/v1/b/iam-bucket", ADMIN, br#"{"versioning":{"enabled":true},"cors":[{"origin":["https://browser.example.invalid"],"method":["GET","HEAD"],"responseHeader":["Content-Type","Content-Disposition","ETag","Content-Range"]}]}"#).ok();
    let before = server.get(ADMIN, METADATA).ok().json();
    let original = server.get(ADMIN, OBJECT);
    original.ok();
    let path = format!(
        "{OBJECT}?generation={}&{QUERY}",
        before["generation"].as_str().unwrap()
    );
    // A retained generation proves overrides do not accidentally use the live record.
    server
        .upload(ADMIN, "iam-bucket", "object", b"new live bytes")
        .ok();
    let live = server.get(ADMIN, METADATA).ok().json();
    let retained = server
        .get(
            ADMIN,
            &format!(
                "{METADATA}?generation={}",
                before["generation"].as_str().unwrap()
            ),
        )
        .ok()
        .json();
    for public in [false, true] {
        if public {
            iam::set(
                &server,
                ADMIN,
                &json!({"bindings":[iam::binding("roles/storage.objectViewer", "allUsers")]}),
            )
            .ok();
        }
        for method in ["GET", "HEAD"] {
            for signed in [false, true] {
                let url = if signed {
                    sign(
                        &server,
                        &method.parse().unwrap(),
                        &path,
                        "viewer@example.invalid",
                        SystemTime::now(),
                        HeaderMap::new(),
                    )
                } else {
                    format!("http://{}{path}", server.address)
                };
                let auth = if signed || public { "" } else { ADMIN };
                let whole = send(&server, method, &url, auth);
                whole.ok();
                assert_eq!(whole.headers["content-type"], "text/csv");
                assert_eq!(whole.headers["content-length"], "11");
                assert_eq!(
                    whole.body,
                    if method == "HEAD" {
                        &b""[..]
                    } else {
                        &b"hello world"[..]
                    }
                );
                let response = send(
                    &server,
                    method,
                    &url,
                    &format!(
                        "{auth}Range: bytes=1-4\r\nOrigin: https://browser.example.invalid\r\n"
                    ),
                );
                assert_eq!(response.status, if method == "HEAD" { 200 } else { 206 });
                assert_eq!(
                    response.body,
                    if method == "HEAD" {
                        &b""[..]
                    } else {
                        &b"ello"[..]
                    }
                );
                assert_eq!(response.headers["content-type"], "text/csv");
                assert_eq!(
                    response.headers["content-disposition"],
                    "attachment; filename=\"a + b.csv\""
                );
                assert_eq!(response.headers["content-range"], "bytes 1-4/11");
                assert_eq!(
                    response.headers["content-length"],
                    if method == "HEAD" { "11" } else { "4" }
                );
                for key in [
                    "etag",
                    "x-goog-hash",
                    "x-goog-generation",
                    "x-goog-metageneration",
                    "cache-control",
                    "content-language",
                    "x-goog-stored-content-length",
                    "x-goog-stored-content-encoding",
                    "x-goog-meta-kept",
                ] {
                    assert_eq!(response.headers[key], original.headers[key], "{key}");
                }
                assert_eq!(
                    response.headers["access-control-allow-origin"],
                    "https://browser.example.invalid"
                );
                assert!(
                    response.headers["access-control-expose-headers"]
                        .to_ascii_lowercase()
                        .contains("content-disposition")
                );
                if signed {
                    for changed in [
                        url.replace("text%2Fcsv", "text%2Fhtml"),
                        url.replace("attachment", "inline"),
                        url.replace("&response-content-type=text%2Fcsv", ""),
                    ] {
                        assert_ne!(changed, url);
                        let response = send(&server, method, &changed, "");
                        assert_eq!(response.status, 403);
                        if method == "GET" {
                            error(&response, 403, "SignatureDoesNotMatch");
                        }
                    }
                    let plain = sign(
                        &server,
                        &method.parse().unwrap(),
                        OBJECT,
                        "viewer@example.invalid",
                        SystemTime::now(),
                        HeaderMap::new(),
                    );
                    let response = send(
                        &server,
                        method,
                        &format!("{plain}&response-content-type=text%2Fhtml"),
                        "",
                    );
                    assert_eq!(response.status, 403);
                    if method == "GET" {
                        error(&response, 403, "SignatureDoesNotMatch");
                    }
                }
            }
        }
    }
    assert_eq!(server.get(ADMIN, METADATA).ok().json(), live);
    assert_eq!(
        server
            .get(
                ADMIN,
                &format!(
                    "{METADATA}?generation={}",
                    before["generation"].as_str().unwrap()
                )
            )
            .ok()
            .json(),
        retained
    );
    let unchanged = server.get(
        ADMIN,
        &format!(
            "{OBJECT}?generation={}",
            before["generation"].as_str().unwrap()
        ),
    );
    unchanged.ok();
    assert_eq!(unchanged.headers["content-type"], "text/plain");
    assert_eq!(unchanged.headers["content-disposition"], "inline");
}

#[test]
fn xml_response_overrides_reject_injection_duplicates_and_unknown_names() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, METADATA).ok().json();
    for parameter in ["response-content-type", "response-content-disposition"] {
        for value in [
            "",
            "%20",
            "%09",
            "%20text%2Fplain",
            "text%2Fplain%20",
            "%09text%2Fplain",
            "text%2Fplain%09",
            "text%2Fplain%0D%0AX-Injected%3Ayes",
            "%0A",
            "%0D",
            "%00",
            "%7F",
            "%01",
        ] {
            let path = format!("{OBJECT}?{parameter}={value}");
            for method in ["GET", "HEAD"] {
                let unsigned = format!("http://{}{path}", server.address);
                let signed = sign(
                    &server,
                    &method.parse().unwrap(),
                    &path,
                    "viewer@example.invalid",
                    SystemTime::now(),
                    HeaderMap::new(),
                );
                for (url, auth) in [(&unsigned, ADMIN), (&signed, "")] {
                    let response = send(&server, method, url, auth);
                    assert_eq!(response.status, 400, "{path}");
                    assert!(!response.headers.contains_key("x-injected"));
                    if method == "HEAD" {
                        assert!(response.body.is_empty());
                    } else {
                        error(&response, 400, "InvalidArgument");
                    }
                }
            }
        }
        let path = format!("{OBJECT}?{parameter}=attachment%3B%20filename%3D%22%C3%A9.csv%22");
        for method in ["GET", "HEAD"] {
            let unsigned = format!("http://{}{path}", server.address);
            let signed = sign(
                &server,
                &method.parse().unwrap(),
                &path,
                "viewer@example.invalid",
                SystemTime::now(),
                HeaderMap::new(),
            );
            for (url, auth) in [(&unsigned, ADMIN), (&signed, "")] {
                let response = send(&server, method, url, auth);
                assert_eq!(response.status, 501);
                if method == "HEAD" {
                    assert!(response.body.is_empty());
                } else {
                    error(&response, 501, "NotImplemented");
                }
            }
        }
    }
    for query in [
        "response-content-type=a&response-content-type=b",
        "response-content-type=a&response%2Dcontent-type=b",
    ] {
        error(
            &server.get(ADMIN, &format!("{OBJECT}?{query}")),
            400,
            "InvalidArgument",
        );
        let url = sign(
            &server,
            &Method::GET,
            OBJECT,
            "viewer@example.invalid",
            SystemTime::now(),
            HeaderMap::new(),
        );
        error(
            &send(&server, "GET", &format!("{url}&{query}"), ""),
            400,
            "InvalidAuthentication",
        );
    }
    for parameter in [
        "response-cache-control",
        "response-content-language",
        "response-content-encoding",
        "response-expires",
        "response-unknown",
        "unknown",
    ] {
        let path = format!("{OBJECT}?{parameter}=value");
        error(&server.get(ADMIN, &path), 501, "NotImplemented");
        let url = sign(
            &server,
            &Method::GET,
            &path,
            "viewer@example.invalid",
            SystemTime::now(),
            HeaderMap::new(),
        );
        error(&send(&server, "GET", &url, ""), 501, "NotImplemented");
    }
    assert_eq!(server.get(ADMIN, METADATA).ok().json(), before);
}

#[test]
fn xml_response_overrides_from_cli_reach_the_server() {
    let (_config, _data, server) = fixture();
    let disposition = "attachment; filename*=UTF-8''%C3%A9%20%2B%20100%25.csv";
    for method in ["GET", "HEAD"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
            .args([
                "url",
                "sign",
                "--base-url",
                &format!("http://{}", server.address),
                "--bucket",
                "iam-bucket",
                "--object",
                "object",
                "--method",
                method,
                "--expires",
                "15m",
                "--signer",
                "viewer@example.invalid",
                "--private-key",
                "fixtures/signing/test-only-private.pem",
                "--query",
                &format!("response-content-disposition={disposition}"),
                "--query",
                "response-content-type=text/csv",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let url = String::from_utf8(output.stdout).unwrap();
        let response = send(&server, method, url.trim(), "");
        response.ok();
        assert_eq!(response.headers["content-type"], "text/csv");
        assert_eq!(response.headers["content-disposition"], disposition);
        assert_eq!(
            response.body,
            if method == "HEAD" {
                &b""[..]
            } else {
                &b"hello world"[..]
            }
        );
    }
    assert_eq!(
        server.get(ADMIN, METADATA).ok().json()["contentType"],
        "application/octet-stream"
    );
}

#[test]
fn xml_response_overrides_are_absent_from_error_responses() {
    let (_config, _data, server) = fixture();
    for (object, headers, status, code) in [
        ("/iam-bucket/missing", "", 404, "NoSuchKey"),
        (
            OBJECT,
            "x-goog-if-generation-match: 0\r\n",
            412,
            "PreconditionFailed",
        ),
        (OBJECT, "Range: bytes=11-\r\n", 416, "InvalidRange"),
    ] {
        let path = format!("{object}?{QUERY}");
        let unsigned = format!("http://{}{path}", server.address);
        let mut signed_headers = HeaderMap::new();
        if status == 412 {
            signed_headers.insert("x-goog-if-generation-match", "0".parse().unwrap());
        }
        let signed = sign(
            &server,
            &Method::GET,
            &path,
            "viewer@example.invalid",
            SystemTime::now(),
            signed_headers,
        );
        for (url, auth) in [(&unsigned, ADMIN), (&signed, "")] {
            let response = send(&server, "GET", url, &format!("{auth}{headers}"));
            error(&response, status, code);
            assert_eq!(
                response.headers["content-type"],
                "application/xml; charset=UTF-8"
            );
            assert!(!response.headers.contains_key("content-disposition"));
        }
    }
}
