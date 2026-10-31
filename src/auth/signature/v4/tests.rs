use super::*;

const DATE: &str = "20261023T120000Z";
const SCOPE: &str = "20261023/auto/storage/goog4_request";

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("host", "localhost".parse().unwrap());
    headers.insert("x-goog-date", DATE.parse().unwrap());
    headers.insert("x-goog-content-sha256", "UNSIGNED-PAYLOAD".parse().unwrap());
    headers.insert(
        "authorization",
        format!(
            "GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=host;x-goog-date, Signature=00"
        )
        .parse()
        .unwrap(),
    );
    headers
}

fn query() -> Uri {
    format!("/bucket/object?X-Goog-Algorithm=GOOG4-HMAC-SHA256&X-Goog-Credential=key%2F{}&X-Goog-Date={DATE}&X-Goog-Expires=60&X-Goog-SignedHeaders=host&X-Goog-Signature=00",encode_query_component(SCOPE)).parse().unwrap()
}

#[test]
fn header_fifteen_minute_and_query_early_expiry_boundaries_are_exact() {
    let time = parse_timestamp(DATE).unwrap();
    let uri: Uri = "/bucket/object".parse().unwrap();
    for offset in [-900i64, 0, 900] {
        let now = if offset < 0 {
            time - Duration::from_secs(offset.unsigned_abs())
        } else {
            time + Duration::from_secs(offset as u64)
        };
        assert!(prepare(&Method::GET, &uri, &headers(), now).is_ok());
    }
    assert!(matches!(
        prepare(
            &Method::GET,
            &uri,
            &headers(),
            time - Duration::from_secs(901)
        ),
        Err(Failure::NotYetValid)
    ));
    assert!(matches!(
        prepare(
            &Method::GET,
            &uri,
            &headers(),
            time + Duration::from_secs(901)
        ),
        Err(Failure::Expired)
    ));
    let mut headers = headers();
    headers.remove("authorization");
    headers.remove("x-goog-date");
    for offset in [-900i64, 0, 59] {
        let now = if offset < 0 {
            time - Duration::from_secs(offset.unsigned_abs())
        } else {
            time + Duration::from_secs(offset as u64)
        };
        assert!(prepare(&Method::GET, &query(), &headers, now).is_ok());
    }
    assert!(matches!(
        prepare(
            &Method::GET,
            &query(),
            &headers,
            time - Duration::from_secs(901)
        ),
        Err(Failure::NotYetValid)
    ));
    assert!(matches!(
        prepare(
            &Method::GET,
            &query(),
            &headers,
            time + Duration::from_secs(60)
        ),
        Err(Failure::Expired)
    ));
}

#[test]
fn credential_prefix_scope_header_duplicates_and_streaming_fields_fail_closed() {
    let now = parse_timestamp(DATE).unwrap();
    let uri: Uri = "/bucket/object".parse().unwrap();
    let good = headers();
    let mut missing_hash = good.clone();
    missing_hash.remove("x-goog-content-sha256");
    assert!(matches!(
        prepare(&Method::GET, &uri, &missing_hash, now),
        Err(Failure::Malformed)
    ));
    for raw in [
        format!(
            "GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, Credential=other/{SCOPE}, SignedHeaders=host;x-goog-date, Signature=00"
        ),
        format!(
            "GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=host;x-goog-date, Signature=00, Unknown=ignored"
        ),
        format!("GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=host;x-goog-date"),
        format!(
            "GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=x-goog-date;host, Signature=00"
        ),
        format!(
            "GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=host;host;x-goog-date, Signature=00"
        ),
        format!("GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=host, Signature=00"),
    ] {
        let mut bad = good.clone();
        bad.insert("authorization", raw.parse().unwrap());
        assert!(matches!(
            prepare(&Method::GET, &uri, &bad, now),
            Err(Failure::Malformed)
        ));
    }
    for (name, value) in [
        ("x-goog-date", DATE),
        ("x-amz-date", DATE),
        ("authorization", "Bearer no"),
        ("x-amz-trailer", "checksum"),
        ("x-amz-decoded-content-length", "0"),
        ("content-encoding", "aws-chunked"),
        (
            "x-goog-content-sha256",
            "STREAMING-AWS4-HMAC-SHA256-PAYLOAD",
        ),
    ] {
        let mut bad = good.clone();
        bad.append(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        assert!(matches!(
            prepare(&Method::GET, &uri, &bad, now),
            Err(Failure::Malformed)
        ));
    }
    let mut query_headers = HeaderMap::new();
    query_headers.insert("host", "localhost".parse().unwrap());
    let raw = query().to_string();
    for bad in [
        raw.replace("X-Goog-Date", "X-Amz-Date"),
        raw.replace("GOOG4-HMAC-SHA256", "AWS4-HMAC-SHA256"),
        raw.replace("storage%2Fgoog4_request", "s3%2Faws4_request"),
        raw.replace("20261023%2Fauto", "20261022%2Fauto"),
        format!("{raw}&%58-Goog-Credential=duplicate"),
        format!("{raw}&GoogleAccessId=mixed"),
        raw.replace("Expires=60", "Expires=604801"),
        raw.replace("Expires=60", "Expires=0"),
    ] {
        assert!(matches!(
            prepare(&Method::GET, &bad.parse().unwrap(), &query_headers, now),
            Err(Failure::Malformed)
        ));
    }
    assert!(matches!(
        prepare(&Method::GET, &query(), &good, now),
        Err(Failure::Malformed)
    ));
    assert!(matches!(
        prepare(&Method::POST, &query(), &query_headers, now),
        Err(Failure::Malformed)
    ));
}

#[test]
fn canonicalization_bounds_resource_use_and_preserves_header_wire_order() {
    let now = parse_timestamp(DATE).unwrap();
    let uri: Uri = "/bucket/object".parse().unwrap();
    let mut h = headers();
    h.insert("authorization",format!("GOOG4-HMAC-SHA256 Credential=key/{SCOPE}, SignedHeaders=host;x-goog-date;x-goog-meta-reviewer, Signature=00").parse().unwrap());
    h.append("x-goog-meta-reviewer", " jane  smith ".parse().unwrap());
    h.append("x-goog-meta-reviewer", "john".parse().unwrap());
    let prepared = prepare(&Method::GET, &uri, &h, now).unwrap();
    h.insert("x-goog-meta-reviewer", "jane smith,john".parse().unwrap());
    assert_eq!(
        prepare(&Method::GET, &uri, &h, now).unwrap().message,
        prepared.message
    );
    h.insert("x-goog-meta-reviewer", "john,jane smith".parse().unwrap());
    assert_ne!(
        prepare(&Method::GET, &uri, &h, now).unwrap().message,
        prepared.message
    );
    let large: Uri = format!("/{}", "a".repeat(16 * 1024)).parse().unwrap();
    assert!(matches!(
        prepare(&Method::GET, &large, &headers(), now),
        Err(Failure::Malformed)
    ));
    h.insert("large", "a".repeat(64 * 1024).parse().unwrap());
    assert!(matches!(
        prepare(&Method::GET, &uri, &h, now),
        Err(Failure::Malformed)
    ));
}
