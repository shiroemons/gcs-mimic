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
    // A mutation response carries the resource etag as a header too (recorded).
    assert_eq!(header(&patched, "etag"), Some(patched_etag.as_str()));
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

const RESOURCE_CACHE: &str = "private, max-age=0, must-revalidate, no-transform";
const NO_STORE: &str = "no-cache, no-store, max-age=0, must-revalidate";
const PRIVATE: &str = "private, max-age=0";

#[test]
fn cache_headers_follow_each_recorded_response_kind() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let missing = "/storage/v1/b/parity/o/missing.txt";
    for (path, cache, pragma) in [
        (OBJECT.to_owned(), RESOURCE_CACHE, None),
        ("/storage/v1/b/parity/o".to_owned(), RESOURCE_CACHE, None),
        ("/storage/v1/b/parity".to_owned(), RESOURCE_CACHE, None),
        (
            "/storage/v1/b?project=project-a".to_owned(),
            RESOURCE_CACHE,
            None,
        ),
        (missing.to_owned(), NO_STORE, Some("no-cache")),
        (format!("{missing}?alt=media"), PRIVATE, None),
        ("/parity/plain.txt".to_owned(), PRIVATE, None),
        ("/parity".to_owned(), PRIVATE, None),
        ("/parity/missing.txt".to_owned(), PRIVATE, None),
    ] {
        let reply = server.request("GET", &path, "", b"");
        assert_eq!(header(&reply, "cache-control"), Some(cache), "{path}");
        assert_eq!(header(&reply, "pragma"), pragma, "{path}");
    }
    // A media download forbids every cache; its Pragma is covered with the media headers.
    for range in ["", "Range: bytes=0-4\r\n"] {
        let media = server.request("GET", &format!("{OBJECT}?alt=media"), range, b"");
        assert_eq!(header(&media, "cache-control"), Some(NO_STORE), "{range}");
    }
    // An unsatisfiable range is a plain error, not a JSON one.
    let unsatisfiable = server.request(
        "GET",
        &format!("{OBJECT}?alt=media"),
        "Range: bytes=100-200\r\n",
        b"",
    );
    assert_eq!(unsatisfiable.status, 416);
    assert_eq!(header(&unsatisfiable, "cache-control"), Some(PRIVATE));
    assert_eq!(header(&unsatisfiable, "pragma"), None);
    // Other media errors keep the JSON error headers.
    let invalid = server.request("GET", &format!("{OBJECT}?alt=media&generation=x"), "", b"");
    assert_eq!(invalid.status, 400);
    assert_eq!(header(&invalid, "cache-control"), Some(NO_STORE));
    assert_eq!(header(&invalid, "pragma"), Some("no-cache"));
}

#[test]
fn missing_object_errors_name_the_object_like_cloud_storage() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let message = "No such object: parity/missing.txt";
    let metadata = server.request("GET", "/storage/v1/b/parity/o/missing.txt", "", b"");
    metadata.status(404);
    assert_eq!(
        header(&metadata, "content-type"),
        Some("application/json; charset=UTF-8")
    );
    let error = metadata.json();
    assert_eq!(error["error"]["code"], 404);
    assert_eq!(error["error"]["message"], message);
    assert_eq!(
        error["error"]["errors"],
        serde_json::json!([{"message": message, "domain": "global", "reason": "notFound"}])
    );
    for method in ["DELETE", "PATCH"] {
        let body: &[u8] = if method == "PATCH" { b"{}" } else { b"" };
        let reply = server.request(method, "/storage/v1/b/parity/o/missing.txt", JSON, body);
        reply.status(404);
        assert_eq!(reply.json()["error"]["message"], message, "{method}");
    }

    let media = server.request(
        "GET",
        "/storage/v1/b/parity/o/missing.txt?alt=media",
        "",
        b"",
    );
    media.status(404);
    assert_eq!(
        header(&media, "content-type"),
        Some("text/html; charset=UTF-8")
    );
    assert_eq!(header(&media, "cache-control"), Some(PRIVATE));
    assert_eq!(header(&media, "pragma"), None);
    assert_eq!(media.body, message);

    let xml = server.request("GET", "/parity/missing.txt", "", b"");
    xml.status(404);
    assert!(
        xml.body.ends_with(
            "<Error><Code>NoSuchKey</Code>\
             <Message>The specified key does not exist.</Message>\
             <Details>No such object: parity/missing.txt</Details></Error>"
        ),
        "{}",
        xml.body
    );
    assert_eq!(
        header(&xml, "content-length"),
        Some(xml.body.len().to_string().as_str())
    );
}

/// An object name is attacker-controlled text that the media 404 returns as `text/html`, so it
/// must be escaped; the JSON and XML forms escape only what their syntax requires.
#[test]
fn missing_object_names_are_escaped_for_the_document_they_appear_in() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let name = "<script>a&b\"c'd</script>";
    let encoded = "%3Cscript%3Ea%26b%22c%27d%3C%2Fscript%3E";

    let media = server.request(
        "GET",
        &format!("/storage/v1/b/parity/o/{encoded}?alt=media"),
        "",
        b"",
    );
    media.status(404);
    assert_eq!(
        header(&media, "content-type"),
        Some("text/html; charset=UTF-8")
    );
    assert_eq!(
        media.body,
        "No such object: parity/&lt;script&gt;a&amp;b&quot;c&#39;d&lt;/script&gt;"
    );
    assert!(!media.body.contains('<'), "{}", media.body);
    assert_eq!(header(&media, "x-content-type-options"), None);
    assert_eq!(header(&media, "content-security-policy"), None);

    // JSON text carries the name verbatim; quotes are escaped by the JSON syntax itself.
    let metadata = server.request("GET", &format!("/storage/v1/b/parity/o/{encoded}"), "", b"");
    metadata.status(404);
    assert_eq!(
        metadata.json()["error"]["message"],
        format!("No such object: parity/{name}")
    );

    // XML character data escapes only ampersand and angle brackets.
    let xml = server.request("GET", &format!("/parity/{encoded}"), "", b"");
    xml.status(404);
    assert!(
        xml.body.contains(
            "<Details>No such object: parity/&lt;script&gt;a&amp;b\"c'd&lt;/script&gt;</Details>"
        ),
        "{}",
        xml.body
    );
}

#[test]
fn unsatisfiable_media_ranges_answer_with_html_text() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let reply = server.request(
        "GET",
        &format!("{OBJECT}?alt=media"),
        "Range: bytes=100-200\r\n",
        b"",
    );
    assert_eq!(reply.status, 416);
    assert_eq!(
        header(&reply, "content-type"),
        Some("text/html; charset=UTF-8")
    );
    assert_eq!(header(&reply, "content-range"), Some("bytes */16"));
    assert_eq!(header(&reply, "cache-control"), Some(PRIVATE));
    assert_eq!(header(&reply, "pragma"), None);
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

/// Asserts the body is the 2-space pretty rendering of its own fields, in order.
fn assert_pretty(reply: &Reply) -> Value {
    let value = reply.json();
    assert_eq!(reply.body, serde_json::to_string_pretty(&value).unwrap());
    assert!(reply.body.starts_with("{\n  \""), "{}", reply.body);
    assert_eq!(
        header(reply, "content-length"),
        Some(reply.body.len().to_string().as_str())
    );
    value
}

#[test]
fn json_bodies_are_pretty_printed_in_recorded_field_order() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    server
        .request(
            "POST",
            "/upload/storage/v1/b/parity/o?uploadType=media&name=second.txt",
            "Content-Type: text/plain\r\n",
            b"second\n",
        )
        .status(200);
    let object = assert_pretty(&server.request("GET", OBJECT, "", b""));
    assert_eq!(
        keys(&object),
        [
            "kind",
            "id",
            "selfLink",
            "mediaLink",
            "name",
            "bucket",
            "generation",
            "metageneration",
            "contentType",
            "storageClass",
            "size",
            "md5Hash",
            "crc32c",
            "etag",
            "timeCreated",
            "updated",
            "timeStorageClassUpdated",
            "timeFinalized",
        ]
    );
    let list =
        assert_pretty(&server.request("GET", "/storage/v1/b/parity/o?maxResults=1", "", b""));
    assert_eq!(keys(&list), ["kind", "nextPageToken", "items"]);
    assert_eq!(keys(&list["items"][0]), keys(&object));
    let error =
        assert_pretty(&server.request("GET", "/storage/v1/b/parity/o/missing.txt", "", b""));
    assert_eq!(keys(&error), ["error"]);
    assert_eq!(keys(&error["error"]), ["code", "message", "errors"]);
    assert_eq!(
        keys(&error["error"]["errors"][0]),
        ["message", "domain", "reason"]
    );
    let explicit = server.request("GET", &format!("{OBJECT}?prettyPrint=true"), "", b"");
    explicit.status(200);
    assert_eq!(assert_pretty(&explicit), object);
    // Writes are pretty too.
    let patched = server.request(
        "PATCH",
        "/storage/v1/b/parity/o/second.txt",
        JSON,
        br#"{"metadata":{"k":"v"}}"#,
    );
    patched.status(200);
    assert_pretty(&patched);

    // prettyPrint=false is the standard Google API parameter for compact output.
    for path in [
        format!("{OBJECT}?prettyPrint=false"),
        "/storage/v1/b/parity/o?prettyPrint=false".to_owned(),
        "/storage/v1/b/parity?prettyPrint=false".to_owned(),
        "/storage/v1/b/parity/o/missing.txt?prettyPrint=false".to_owned(),
    ] {
        let compact = server.request("GET", &path, "", b"");
        let value = compact.json();
        assert_eq!(compact.body, value.to_string(), "{path}");
        assert!(!compact.body.contains('\n'), "{path}");
    }
    // Compact output keeps the recorded field order.
    let compact = server.request("GET", &format!("{OBJECT}?prettyPrint=false"), "", b"");
    assert_eq!(keys(&compact.json()), keys(&object));
    // A value that is not a boolean is still rejected.
    let invalid = server.request("GET", &format!("{OBJECT}?prettyPrint=maybe"), "", b"");
    assert_eq!(invalid.status, 400);
}

#[test]
fn legacy_profile_keeps_compact_bodies() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--compat-profile", "legacy-emulator"]);
    setup(&server);
    for path in [OBJECT, "/storage/v1/b/parity", "/storage/v1/b/parity/o"] {
        let reply = server.request("GET", path, "", b"");
        // The legacy bucket body keeps fake-gcs-server's trailing newline.
        assert_eq!(reply.body.trim_end(), reply.json().to_string(), "{path}");
        assert!(!reply.body.contains("\n  "), "{path}");
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/parity", "", b"")
            .json()
            .get("generation"),
        None
    );
    // The reference emulator's bucket shape has no satisfiesPZI.
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/parity", "", b"")
            .json()
            .get("satisfiesPZI"),
        None
    );
}

#[test]
fn batch_parts_choose_their_own_format() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let part = |id: &str, query: &str| {
        format!(
            "--b\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\nContent-ID: <{id}>\r\n\r\n\
             GET /storage/v1/b/parity/o/plain.txt{query} HTTP/1.1\r\n\r\n\r\n"
        )
    };
    let body = format!(
        "{}{}--b--\r\n",
        part("pretty", ""),
        part("compact", "?prettyPrint=false")
    );
    let reply = server.request(
        "POST",
        "/batch/storage/v1",
        "Content-Type: multipart/mixed; boundary=b\r\n",
        body.as_bytes(),
    );
    reply.status(200);
    let pretty_at = reply.body.find("response-pretty").unwrap();
    let compact_at = reply.body.find("response-compact").unwrap();
    let pretty = &reply.body[pretty_at..compact_at];
    let compact = &reply.body[compact_at..];
    assert!(
        pretty.contains("{\n  \"kind\": \"storage#object\""),
        "{pretty}"
    );
    assert!(
        compact.contains("{\"kind\":\"storage#object\""),
        "{compact}"
    );
    assert!(!compact.contains("{\n  \"kind\""), "{compact}");

    // An outer prettyPrint=false is inherited by parts that do not set their own.
    let inherited = server.request(
        "POST",
        "/batch/storage/v1?prettyPrint=false",
        "Content-Type: multipart/mixed; boundary=b\r\n",
        format!("{}--b--\r\n", part("a", "")).as_bytes(),
    );
    inherited.status(200);
    assert!(inherited.body.contains("{\"kind\":\"storage#object\""));
    // A part's own value overrides the outer one.
    let overridden = server.request(
        "POST",
        "/batch/storage/v1?prettyPrint=false",
        "Content-Type: multipart/mixed; boundary=b\r\n",
        format!("{}--b--\r\n", part("a", "?prettyPrint=true")).as_bytes(),
    );
    overridden.status(200);
    assert!(
        overridden
            .body
            .contains("{\n  \"kind\": \"storage#object\"")
    );
}

#[test]
fn bucket_resources_carry_recorded_fields_in_recorded_order() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let created = server.request(
        "POST",
        "/storage/v1/b?project=123456",
        JSON,
        br#"{"name":"uniform","location":"us-central1","iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#,
    );
    created.status(200);
    let bucket = assert_pretty(&server.request("GET", "/storage/v1/b/uniform", "", b""));
    assert_eq!(bucket, created.json());
    assert_eq!(
        keys(&bucket),
        [
            "kind",
            "selfLink",
            "id",
            "name",
            "projectNumber",
            "generation",
            "metageneration",
            "location",
            "storageClass",
            "etag",
            "timeCreated",
            "updated",
            "iamConfiguration",
            "locationType",
            "satisfiesPZI",
        ]
    );
    assert_eq!(bucket["satisfiesPZI"], true);
    assert_eq!(bucket["projectNumber"], "123456");
    // Cloud Storage renders times with exactly three fractional digits.
    for field in ["timeCreated", "updated"] {
        let time = bucket[field].as_str().unwrap();
        assert!(
            time.len() == 24 && &time[19..20] == "." && time.ends_with('Z'),
            "{field} {time}"
        );
    }
    // The generation is the catalog's stored incarnation counter, not a derived value.
    let generation = bucket["generation"].as_str().unwrap();
    assert!(
        !generation.is_empty() && generation.bytes().all(|byte| byte.is_ascii_digit()),
        "{generation}"
    );
    // Bucket generations are nanosecond-scale, 19 digits like Cloud Storage's.
    assert_eq!(generation.len(), 19, "{generation}");
    assert_eq!(
        keys(&bucket["iamConfiguration"]),
        [
            "bucketPolicyOnly",
            "uniformBucketLevelAccess",
            "publicAccessPrevention"
        ]
    );
    let uniform = &bucket["iamConfiguration"]["uniformBucketLevelAccess"];
    assert_eq!(uniform["enabled"], true);
    assert_eq!(bucket["iamConfiguration"]["bucketPolicyOnly"], *uniform);

    let list =
        assert_pretty(&server.request("GET", "/storage/v1/b?project=123456&maxResults=1", "", b""));
    assert_eq!(keys(&list), ["kind", "items"]);
    assert_eq!(list["items"][0], bucket);
}

#[test]
fn xml_listing_matches_the_recorded_document() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let plain = setup(&server);
    let second = server.request(
        "POST",
        "/upload/storage/v1/b/parity/o?uploadType=media&name=second.txt",
        "Content-Type: text/plain\r\n",
        b"second\n",
    );
    second.status(200);
    let contents = |object: &Value, md5: &str| {
        format!(
            "<Contents><Key>{}</Key><Generation>{}</Generation><MetaGeneration>1</MetaGeneration>\
             <LastModified>{}</LastModified><ETag>\"{md5}\"</ETag><Size>{}</Size></Contents>",
            object["name"].as_str().unwrap(),
            object["generation"].as_str().unwrap(),
            object["updated"].as_str().unwrap(),
            object["size"].as_str().unwrap(),
        )
    };
    let list = server.request("GET", "/parity?max-keys=5", "", b"");
    list.status(200);
    assert_eq!(
        list.body,
        format!(
            "<?xml version='1.0' encoding='UTF-8'?>\
             <ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'>\
             <Name>parity</Name><Prefix></Prefix><Marker></Marker><MaxKeys>5</MaxKeys>\
             <IsTruncated>false</IsTruncated>{}{}</ListBucketResult>",
            contents(&plain, "dd2e843409823500b8dbd87f539398e5"),
            contents(&second.json(), "59d0d19fc45ca69230d858f60a5557f8"),
        )
    );
    assert_eq!(
        header(&list, "content-type"),
        Some("application/xml; charset=UTF-8")
    );
    assert_eq!(header(&list, "x-goog-metageneration"), Some("1"));
    assert_eq!(header(&list, "cache-control"), Some(PRIVATE));
    assert_eq!(
        header(&list, "content-length"),
        Some(list.body.len().to_string().as_str())
    );
    // The header reports the bucket's current metageneration.
    server
        .request(
            "PATCH",
            "/storage/v1/b/parity",
            JSON,
            br#"{"labels":{"k":"v"}}"#,
        )
        .status(200);
    let relabeled = server.request("GET", "/parity", "", b"");
    assert_eq!(header(&relabeled, "x-goog-metageneration"), Some("2"));
    // A requested delimiter is still echoed.
    let delimited = server.request("GET", "/parity?delimiter=%2F", "", b"");
    delimited.status(200);
    assert!(
        delimited.body.contains("<Delimiter>/</Delimiter>"),
        "{}",
        delimited.body
    );
    // A truncated page puts its continuation marker between the request markers and MaxKeys.
    let truncated = server.request("GET", "/parity?max-keys=1&marker=a", "", b"");
    truncated.status(200);
    assert!(
        truncated.body.contains(
            "<Marker>a</Marker><NextMarker>plain.txt</NextMarker>\
             <MaxKeys>1</MaxKeys><IsTruncated>true</IsTruncated>"
        ),
        "{}",
        truncated.body
    );
    // A listing that names no usable max-keys echoes no MaxKeys, and a delimiter follows the
    // marker.
    for query in ["", "?max-keys=-1", "?max-keys=abc"] {
        let unlimited = server.request("GET", &format!("/parity{query}"), "", b"");
        unlimited.status(200);
        assert!(
            unlimited
                .body
                .contains("<Marker></Marker><IsTruncated>false</IsTruncated>"),
            "{query}: {}",
            unlimited.body
        );
    }
    let delimited = server.request("GET", "/parity?delimiter=%2F&prefix=p", "", b"");
    assert!(
        delimited.body.contains(
            "<Prefix>p</Prefix><Marker></Marker><Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>"
        ),
        "{}",
        delimited.body
    );
}

#[test]
fn xml_documents_use_the_single_quoted_declaration() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let missing = server.request("GET", "/parity/missing.txt", "", b"");
    assert_eq!(
        missing.body,
        "<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code>\
         <Message>The specified key does not exist.</Message>\
         <Details>No such object: parity/missing.txt</Details></Error>"
    );
    for path in ["/parity", "/parity?versions"] {
        let reply = server.request("GET", path, "", b"");
        reply.status(200);
        assert!(
            reply
                .body
                .starts_with("<?xml version='1.0' encoding='UTF-8'?>"),
            "{path}: {}",
            reply.body
        );
        assert!(!reply.body.contains("xmlns=\""), "{path}: {}", reply.body);
    }
    // Character data keeps literal quotes; only ampersand and angle brackets are escaped.
    server
        .request(
            "POST",
            "/upload/storage/v1/b/parity/o?uploadType=media&name=a%20%26%3C%22%27.txt",
            "Content-Type: text/plain\r\n",
            b"x",
        )
        .status(200);
    let listed = server.request("GET", "/parity?prefix=a", "", b"");
    assert!(
        listed.body.contains("a &amp;&lt;\"'.txt"),
        "{}",
        listed.body
    );
}

#[test]
fn object_inserts_and_json_write_errors_forbid_every_cache() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let media = server.request(
        "POST",
        "/upload/storage/v1/b/parity/o?uploadType=media&name=again.txt",
        "Content-Type: text/plain\r\n",
        b"again\n",
    );
    media.status(200);
    let multipart = server.request(
        "POST",
        "/upload/storage/v1/b/parity/o?uploadType=multipart",
        "Content-Type: multipart/related; boundary=b\r\n",
        b"--b\r\nContent-Type: application/json\r\n\r\n{\"name\":\"multi.txt\"}\r\n--b\r\nContent-Type: text/plain\r\n\r\nmulti\r\n--b--\r\n",
    );
    multipart.status(200);
    for (kind, reply) in [("media", &media), ("multipart", &multipart)] {
        assert_eq!(header(reply, "cache-control"), Some(NO_STORE), "{kind}");
        assert_eq!(header(reply, "pragma"), Some("no-cache"), "{kind}");
    }
    let rejected = server.request(
        "POST",
        "/storage/v1/b?project=project-a",
        JSON,
        br#"{"name":"parity"}"#,
    );
    assert_eq!(rejected.status, 409);
    assert_eq!(header(&rejected, "cache-control"), Some(NO_STORE));
    assert_eq!(header(&rejected, "pragma"), Some("no-cache"));
}

#[test]
fn json_api_responses_carry_the_recorded_cache_envelope() {
    const NO_CACHE: &str = "no-cache, no-store, max-age=0, must-revalidate";
    const REVALIDATE: &str = "private, max-age=0, must-revalidate, no-transform";
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);

    let read = server.request("GET", "/storage/v1/b/parity", "", b"");
    read.status(200);
    assert_eq!(header(&read, "cache-control"), Some(REVALIDATE));
    assert_eq!(header(&read, "pragma"), None);
    assert_eq!(header_values(&read, "vary").join(", "), "Origin, X-Origin");
    assert_ne!(
        header(&read, "expires"),
        Some("Mon, 01 Jan 1990 00:00:00 GMT")
    );
    assert!(header(&read, "expires").is_some());

    let created = server.request(
        "POST",
        "/storage/v1/b?project=project-a",
        JSON,
        br#"{"name":"parity-two"}"#,
    );
    let missing = server.request("GET", "/storage/v1/b/parity-absent", "", b"");
    missing.status(404);
    let deleted = server.request("DELETE", "/storage/v1/b/parity-two", "", b"");
    deleted.status(204);
    for reply in [&created, &missing, &deleted] {
        assert_eq!(header(reply, "cache-control"), Some(NO_CACHE));
        assert_eq!(
            header(reply, "expires"),
            Some("Mon, 01 Jan 1990 00:00:00 GMT")
        );
        assert_eq!(header(reply, "pragma"), Some("no-cache"));
        assert_eq!(header_values(reply, "vary").join(", "), "Origin, X-Origin");
    }
    assert_eq!(header(&deleted, "content-type"), Some("application/json"));

    // A field-selected body keeps the full resource's ETag header.
    let selected = server.request("GET", "/storage/v1/b/parity?fields=name", "", b"");
    assert_eq!(
        header(&selected, "etag"),
        Some(read.json()["etag"].as_str().unwrap())
    );
}

#[test]
fn error_items_name_the_rejected_parameter_or_header() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let item = |reply: &Reply| reply.json()["error"]["errors"][0].clone();

    let bad_size = server.request(
        "GET",
        "/storage/v1/b?project=project-a&maxResults=abc",
        "",
        b"",
    );
    bad_size.status(400);
    assert_eq!(item(&bad_size)["location"], "maxResults");
    assert_eq!(item(&bad_size)["locationType"], "parameter");

    let missing_project = server.request("GET", "/storage/v1/b", "", b"");
    missing_project.status(400);
    assert_eq!(item(&missing_project)["location"], "project");

    let bad_projection = server.request("GET", &format!("{OBJECT}?projection=bogus"), "", b"");
    bad_projection.status(400);
    assert_eq!(item(&bad_projection)["location"], "projection");

    let unbalanced = server.request("GET", &format!("{OBJECT}?fields=metadata%28a"), "", b"");
    unbalanced.status(400);
    assert_eq!(item(&unbalanced)["location"], "fields");

    // A failed precondition names the header even when the condition was a query parameter.
    let failed = server.request("DELETE", &format!("{OBJECT}?ifGenerationMatch=1"), "", b"");
    failed.status(412);
    assert_eq!(item(&failed)["location"], "If-Match");
    assert_eq!(item(&failed)["locationType"], "header");

    // A rejection that is not about one input carries no location.
    let missing = server.request("GET", "/storage/v1/b/parity/o/absent.txt", "", b"");
    missing.status(404);
    assert!(item(&missing).get("location").is_none());
}

#[test]
fn xml_object_errors_and_headers_match_the_recorded_responses() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    // A PUT sends no Cache-Control and varies by Origin; its failures say the same.
    let stale = server.request(
        "PUT",
        "/parity/plain.txt",
        "x-goog-if-generation-match: 1\r\n",
        b"x",
    );
    stale.status(412);
    assert!(stale.body.contains(
        "<Message>At least one of the pre-conditions you specified did not hold.</Message>"
    ));
    assert_eq!(header(&stale, "cache-control"), None);
    assert_eq!(header(&stale, "vary"), Some("Origin"));
    // A DELETE of an absent object names it, and is private with an Expires.
    let absent = server.request("DELETE", "/parity/absent.txt", "", b"");
    absent.status(404);
    assert!(
        absent
            .body
            .contains("<Details>No such object: parity/absent.txt</Details>")
    );
    assert_eq!(header(&absent, "cache-control"), Some(PRIVATE));
    assert!(header(&absent, "expires").is_some());
    assert_eq!(header(&absent, "vary"), Some("Origin"));
    // A copy from an absent source names the source, not the destination.
    let copy = server.request(
        "PUT",
        "/parity/copy.txt",
        "x-goog-copy-source: /parity/nope.txt\r\n",
        b"",
    );
    copy.status(404);
    assert!(
        copy.body
            .contains("<Details>No such object: parity/nope.txt</Details>")
    );
    // A non-matching copy-source ETag is a 400, not a 412.
    let etag = server.request(
        "PUT",
        "/parity/copy.txt",
        "x-goog-copy-source: /parity/plain.txt\r\nx-goog-copy-source-if-match: \"bogus\"\r\n",
        b"",
    );
    etag.status(400);
    assert!(
        etag.body
            .contains("<Code>InvalidArgument</Code><Message>Invalid argument.</Message>")
    );
    // Several ranges are refused with the object's validators; a malformed one is ignored.
    let multi = server.request("GET", "/parity/plain.txt", "Range: bytes=0-1,3-4\r\n", b"");
    multi.status(400);
    assert!(
        multi
            .body
            .contains("<Details>Multiple ranges are not supported; Range:bytes=0-1,3-4</Details>")
    );
    assert_eq!(header(&multi, "accept-ranges"), Some("bytes"));
    assert!(header(&multi, "etag").is_some());
    server
        .request("GET", "/parity/plain.txt", "Range: bytes=abc\r\n", b"")
        .status(200);
    // An unknown query parameter or x-goog header is ignored on an object read.
    server
        .request("GET", "/parity/plain.txt?bogusParameter=1", "", b"")
        .status(200);
    server
        .request(
            "GET",
            "/parity/plain.txt",
            "x-goog-bogus-header: 1\r\n",
            b"",
        )
        .status(200);
    // A successful PUT is an empty text/html response with one x-goog-hash field per algorithm.
    let put = server.request("PUT", "/parity/new.txt", "", b"hi");
    put.status(200);
    assert_eq!(
        header(&put, "content-type"),
        Some("text/html; charset=UTF-8")
    );
    assert_eq!(header_values(&put, "x-goog-hash").len(), 2);
    // A declared CRC32C that does not match is a CrcMismatch.
    let crc = server.request(
        "PUT",
        "/parity/bad.txt",
        "x-goog-hash: crc32c=AAAAAA==\r\n",
        b"hi",
    );
    crc.status(400);
    assert!(crc.body.contains("<Code>CrcMismatch</Code>"));
}

#[test]
fn xml_range_errors_of_a_public_object_are_private() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    server
        .request(
            "POST",
            "/upload/storage/v1/b/parity/o?uploadType=media&name=pub.txt&predefinedAcl=publicRead",
            "Content-Type: text/plain\r\n",
            b"0123456789",
        )
        .status(200);
    let whole = server.request("GET", "/parity/pub.txt", "", b"");
    whole.status(200);
    assert_eq!(
        header(&whole, "cache-control"),
        Some("public, max-age=3600")
    );
    for range in ["bytes=0-1,3-4", "bytes=20-30"] {
        let refused = server.request(
            "GET",
            "/parity/pub.txt",
            &format!("Range: {range}\r\n"),
            b"",
        );
        assert!(matches!(refused.status, 400 | 416), "{range}");
        assert_eq!(header(&refused, "cache-control"), Some(PRIVATE), "{range}");
        assert!(header(&refused, "expires").is_some(), "{range}");
    }
}
