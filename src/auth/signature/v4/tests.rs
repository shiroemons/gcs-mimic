use super::*;

const DATE: &str = "20261023T120000Z";
const SCOPE: &str = "20261023/auto/storage/goog4_request";
const AWS_SCOPE: &str = "20261023/auto/s3/aws4_request";
const AWS_SCOPE_ENCODED: &str = "20261023%2Fauto%2Fs3%2Faws4_request";

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

fn aws_query(expires: &str) -> Uri {
    format!("/bucket/object?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=key%2F{AWS_SCOPE_ENCODED}&X-Amz-Date={DATE}&X-Amz-Expires={expires}&X-Amz-SignedHeaders=host&X-Amz-Signature=00").parse().unwrap()
}

fn host_only() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("host", "localhost".parse().unwrap());
    headers
}

/// `uri` with every query parameter name passed through `rename`.
fn spelled(uri: &Uri, rename: impl Fn(&str) -> String) -> Uri {
    let pairs = uri
        .query()
        .unwrap()
        .split('&')
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap();
            format!("{}={value}", rename(name))
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{pairs}", uri.path()).parse().unwrap()
}

/// The string to sign for a GET of the test object over `query`, written out here rather than
/// derived from the canonicalizer under test.
fn get_message(algorithm: &str, scope: &str, query: &str) -> String {
    string_to_sign(
        algorithm,
        DATE,
        scope,
        &format!("GET\n/bucket/object\n{query}\nhost:localhost\n\nhost\nUNSIGNED-PAYLOAD"),
    )
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
        raw.replace("Expires=60", "Expires="),
    ] {
        assert!(matches!(
            prepare(&Method::GET, &bad.parse().unwrap(), &query_headers, now),
            Err(Failure::Malformed)
        ));
    }
    for (expires, value) in [
        ("604801", InvalidExpires::Other),
        ("0", InvalidExpires::Zero),
    ] {
        let bad = raw.replace("Expires=60", &format!("Expires={expires}"));
        assert_eq!(
            prepare(&Method::GET, &bad.parse().unwrap(), &query_headers, now).err(),
            Some(Failure::ExpiresOutOfRange(ExpiresParameter::Google, value))
        );
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
fn an_out_of_range_expiry_names_the_parameter_of_its_algorithm() {
    let now = parse_timestamp(DATE).unwrap();
    let headers = host_only();
    assert!(prepare(&Method::GET, &aws_query("60"), &headers, now).is_ok());
    for (expires, value) in [
        ("0", InvalidExpires::Zero),
        ("00", InvalidExpires::Zero),
        ("604801", InvalidExpires::Other),
        ("-1", InvalidExpires::Other),
        ("abc", InvalidExpires::Other),
    ] {
        assert_eq!(
            prepare(&Method::GET, &aws_query(expires), &headers, now).err(),
            Some(Failure::ExpiresOutOfRange(ExpiresParameter::Aws, value)),
            "{expires}"
        );
    }
    assert!(matches!(
        prepare(&Method::GET, &aws_query(""), &headers, now),
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

#[test]
fn signed_url_post_only_starts_a_resumable_upload_with_a_signed_header() {
    let now = parse_timestamp(DATE).unwrap();
    let signed: Uri = query()
        .to_string()
        .replace(
            "SignedHeaders=host",
            "SignedHeaders=host%3Bx-goog-resumable",
        )
        .parse()
        .unwrap();
    let mut start = HeaderMap::new();
    start.insert("host", "localhost".parse().unwrap());
    start.insert("x-goog-resumable", "start".parse().unwrap());
    assert!(prepare(&Method::POST, &signed, &start, now).is_ok());
    // Without the header the signed header is missing, which is a signature mismatch.
    let mut missing = start.clone();
    missing.remove("x-goog-resumable");
    assert!(matches!(
        prepare(&Method::POST, &signed, &missing, now),
        Err(Failure::Mismatch)
    ));
    // The header must be signed, carry the one documented value and accompany only POST.
    assert!(matches!(
        prepare(&Method::POST, &query(), &start, now),
        Err(Failure::Malformed)
    ));
    let mut stop = start.clone();
    stop.insert("x-goog-resumable", "stop".parse().unwrap());
    let mut repeated = start.clone();
    repeated.append("x-goog-resumable", "start".parse().unwrap());
    for headers in [&stop, &repeated] {
        assert!(matches!(
            prepare(&Method::POST, &signed, headers, now),
            Err(Failure::Malformed)
        ));
    }
    for method in [Method::GET, Method::PUT] {
        assert!(matches!(
            prepare(&method, &signed, &start, now),
            Err(Failure::Malformed)
        ));
    }
}

#[test]
fn parameter_names_parse_to_their_family_and_field_in_any_ascii_case() {
    for (family, prefix) in [(Family::Google, "X-Goog-"), (Family::Aws, "X-Amz-")] {
        for (field, suffix) in [
            (Field::Algorithm, "Algorithm"),
            (Field::Credential, "Credential"),
            (Field::Date, "Date"),
            (Field::Expires, "Expires"),
            (Field::SignedHeaders, "SignedHeaders"),
            (Field::Signature, "Signature"),
        ] {
            let name = format!("{prefix}{suffix}");
            for spelling in [
                name.clone(),
                name.to_ascii_lowercase(),
                name.to_ascii_uppercase(),
            ] {
                assert_eq!(
                    spelling.parse::<V4Parameter>(),
                    Ok(V4Parameter { family, field }),
                    "{spelling}"
                );
            }
        }
    }
    for name in [
        "",
        "x-goog-",
        "x-goog-other",
        "x-goog-expires2",
        "xgoog-date",
        "GoogleAccessId",
        "Expires",
        "Signature",
        // A prefix length can fall inside a multi-byte character.
        "x-gooé-date",
    ] {
        assert_eq!(name.parse::<V4Parameter>(), Err(NotV4Parameter), "{name}");
    }
}

#[test]
fn query_names_match_in_any_ascii_case_and_are_signed_as_sent() {
    let now = parse_timestamp(DATE).unwrap();
    let credential = format!("key%2F{}", encode_query_component(SCOPE));
    // Sorted by the bytes of the names as sent, so uppercase letters come first.
    let mixed = |name: &str| match name {
        "X-Goog-Credential" => "x-GOOG-credential".to_owned(),
        "X-Goog-Date" => "x-goog-date".to_owned(),
        "X-Goog-SignedHeaders" => "X-Goog-signedheaders".to_owned(),
        "X-Goog-Signature" => "x-goog-SIGNATURE".to_owned(),
        _ => name.to_owned(),
    };
    for (uri, expected) in [
        (
            spelled(&query(), str::to_ascii_lowercase),
            format!(
                "x-goog-algorithm=GOOG4-HMAC-SHA256&x-goog-credential={credential}&x-goog-date={DATE}&x-goog-expires=60&x-goog-signedheaders=host"
            ),
        ),
        (
            spelled(&query(), str::to_ascii_uppercase),
            format!(
                "X-GOOG-ALGORITHM=GOOG4-HMAC-SHA256&X-GOOG-CREDENTIAL={credential}&X-GOOG-DATE={DATE}&X-GOOG-EXPIRES=60&X-GOOG-SIGNEDHEADERS=host"
            ),
        ),
        (
            spelled(&query(), mixed),
            format!(
                "X-Goog-Algorithm=GOOG4-HMAC-SHA256&X-Goog-Expires=60&X-Goog-signedheaders=host&x-GOOG-credential={credential}&x-goog-date={DATE}"
            ),
        ),
    ] {
        let prepared = prepare(&Method::GET, &uri, &host_only(), now).unwrap();
        assert_eq!(prepared.signer, "key", "{uri}");
        assert_eq!(prepared.signature, [0], "{uri}");
        assert_eq!(
            prepared.message,
            get_message("GOOG4-HMAC-SHA256", SCOPE, &expected),
            "{uri}"
        );
    }
    let aws = spelled(&aws_query("60"), str::to_ascii_lowercase);
    let prepared = prepare(&Method::GET, &aws, &host_only(), now).unwrap();
    assert_eq!(prepared.signer, "key");
    assert_eq!(
        prepared.message,
        get_message(
            "AWS4-HMAC-SHA256",
            AWS_SCOPE,
            &format!(
                "x-amz-algorithm=AWS4-HMAC-SHA256&x-amz-credential=key%2F{AWS_SCOPE_ENCODED}&x-amz-date={DATE}&x-amz-expires=60&x-amz-signedheaders=host"
            ),
        )
    );
}

#[test]
fn the_case_of_a_query_name_is_part_of_what_is_signed() {
    let now = parse_timestamp(DATE).unwrap();
    let message = |uri: &Uri| {
        prepare(&Method::GET, uri, &host_only(), now)
            .unwrap()
            .message
    };
    assert_ne!(
        message(&query()),
        message(&spelled(&query(), str::to_ascii_lowercase))
    );
}

#[test]
fn query_names_differing_only_in_case_and_mixed_families_are_malformed() {
    let now = parse_timestamp(DATE).unwrap();
    let raw = query().to_string();
    let lower = spelled(&query(), str::to_ascii_lowercase).to_string();
    let aws = aws_query("60").to_string();
    let aws_lower = spelled(&aws_query("60"), str::to_ascii_lowercase).to_string();
    let malformed = |bad: String| {
        let uri = bad.parse().unwrap();
        assert!(
            matches!(
                prepare(&Method::GET, &uri, &host_only(), now),
                Err(Failure::Malformed)
            ),
            "{bad}"
        );
    };
    let repeated = |bad: String, name: &'static str| {
        let uri = bad.parse().unwrap();
        assert!(
            matches!(
                prepare(&Method::GET, &uri, &host_only(), now),
                Err(Failure::RepeatedParameter(reported)) if reported.to_string() == name
            ),
            "{bad}"
        );
    };
    for (name, value) in [
        ("x-goog-algorithm", "GOOG4-HMAC-SHA256"),
        ("x-goog-credential", "other"),
        ("x-goog-date", DATE),
        ("x-goog-expires", "60"),
        ("x-goog-signedheaders", "host"),
        ("x-goog-signature", "00"),
    ] {
        repeated(format!("{raw}&{name}={value}"), name);
        repeated(
            format!("{lower}&{}={value}", name.to_ascii_uppercase()),
            name,
        );
    }
    for name in ["x-amz-date", "X-AMZ-DATE"] {
        repeated(format!("{aws}&{name}={DATE}"), "x-amz-date");
    }
    repeated(format!("{aws_lower}&X-AMZ-DATE={DATE}"), "x-amz-date");
    // The same spelling twice already fails where the query is parsed.
    malformed(format!("{aws_lower}&x-amz-date={DATE}"));
    for bad in [
        format!("{lower}&x-amz-date={DATE}"),
        lower.replace("x-goog-date", "x-amz-date"),
        lower.replace("GOOG4-HMAC-SHA256", "AWS4-HMAC-SHA256"),
        aws_lower.replace("AWS4-HMAC-SHA256", "GOOG4-HMAC-SHA256"),
        lower.replace("&x-goog-credential=", "&x-goog-missing="),
        lower.replace("expires=60", "expires="),
        // V2 names are not V4 parameters in any spelling, and a V4 URL cannot carry them.
        format!("{lower}&expires=1"),
        format!("{lower}&signature=x"),
        format!("{lower}&googleaccessid=x"),
    ] {
        malformed(bad);
    }
    assert_eq!(
        prepare(
            &Method::GET,
            &lower.replace("expires=60", "expires=0").parse().unwrap(),
            &host_only(),
            now
        )
        .err(),
        Some(Failure::ExpiresOutOfRange(
            ExpiresParameter::Google,
            InvalidExpires::Zero
        ))
    );
    assert!(matches!(
        prepare(
            &Method::GET,
            &lower.parse().unwrap(),
            &host_only(),
            now + Duration::from_secs(60)
        ),
        Err(Failure::Expired)
    ));
}
