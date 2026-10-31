use super::*;

fn headers(kind: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("date", "Tue, 27 Mar 2007 19:36:42 GMT".parse().unwrap());
    headers.insert(
        "authorization",
        format!("{kind} key:AAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .parse()
            .unwrap(),
    );
    headers
}

#[test]
fn query_and_header_profiles_keep_their_documented_canonical_differences() {
    let now = parse_timestamp("20070327T193642Z").unwrap();
    for kind in ["GOOG1", "AWS"] {
        let mut input = headers(kind);
        let name = if kind == "GOOG1" {
            "x-goog-meta-k"
        } else {
            "x-amz-meta-k"
        };
        input.insert(name, "  a  b  ".parse().unwrap());
        input.append(name, "c".parse().unwrap());
        let prepared = prepare(
            &Method::GET,
            &"/bucket/object?response-content-type=text%2Fplain&uploadId=a%2Fb&prefix=ignored"
                .parse()
                .unwrap(),
            &input,
            now,
            &Resource::PathStyle,
        )
        .unwrap();
        assert_eq!(
            prepared.message,
            format!(
                "GET\n\n\nTue, 27 Mar 2007 19:36:42 GMT\n{name}:a  b,c\n/bucket/object?response-content-type=text/plain&uploadId=a/b"
            )
        );
        if kind == "GOOG1" {
            input.remove("authorization");
            let query=super::super::prepare_resource(&Method::GET,&"/bucket/object?response-content-type=text%2Fplain&uploadId=a%2Fb&prefix=ignored&GoogleAccessId=key&Expires=1175139620&Signature=AAAAAAAAAAAAAAAAAAAAAAAAAAA%3D".parse().unwrap(),&input,now,&Resource::PathStyle).unwrap();
            assert_eq!(
                query.message,
                "GET\n\n\n1175139620\nx-goog-meta-k:a b,c\n/bucket/object?uploadId=a%2Fb"
            );
        }
    }
}

#[test]
fn legacy_query_decoding_preserves_plus_and_bounded_duplicate_validation() {
    let now = parse_timestamp("20070327T193642Z").unwrap();
    for kind in ["GOOG1", "AWS"] {
        let input = headers(kind);
        for (encoded, decoded) in [
            ("image/svg+xml", "image/svg+xml"),
            ("image%2Fsvg%2Bxml", "image/svg+xml"),
            ("image%2Fsvg%20xml", "image/svg xml"),
        ] {
            let uri = format!("/bucket/object?response-content-type={encoded}")
                .parse()
                .unwrap();
            assert_eq!(
                prepare(&Method::GET, &uri, &input, now, &Resource::PathStyle)
                    .unwrap()
                    .message,
                format!(
                    "GET\n\n\nTue, 27 Mar 2007 19:36:42 GMT\n/bucket/object?response-content-type={decoded}"
                )
            );
        }
        for bad in ["a=1&%61=2", "a=%", "a=%ff", "%ff=a"] {
            assert!(xml_query(bad, &input).is_err(), "{bad}");
        }
        assert!(xml_query("", &input).unwrap().is_empty());
        assert!(xml_query(&"x".repeat(16 * 1024), &input).is_ok());
        assert!(xml_query(&"x".repeat(16 * 1024 + 1), &input).is_err());
    }
    assert_eq!(xml_query("a=+", &HeaderMap::new()).unwrap()["a"], " ");
}

#[test]
fn legacy_header_clock_boundaries_and_alternate_date_precedence_are_explicit() {
    let now = parse_timestamp("20070327T193642Z").unwrap();
    let uri: Uri = "/bucket/object".parse().unwrap();
    for kind in ["GOOG1", "AWS"] {
        for offset in [-900_i64, 0, 900] {
            let at = if offset < 0 {
                now - Duration::from_secs(offset.unsigned_abs())
            } else {
                now + Duration::from_secs(offset as u64)
            };
            assert!(prepare(&Method::GET, &uri, &headers(kind), at, &Resource::PathStyle).is_ok());
        }
        assert!(matches!(
            prepare(
                &Method::GET,
                &uri,
                &headers(kind),
                now + Duration::from_secs(901),
                &Resource::PathStyle
            ),
            Err(Failure::Expired)
        ));
        assert!(matches!(
            prepare(
                &Method::GET,
                &uri,
                &headers(kind),
                now - Duration::from_secs(901),
                &Resource::PathStyle
            ),
            Err(Failure::NotYetValid)
        ));
        let mut alternate = headers(kind);
        alternate.insert("date", "ignored date value".parse().unwrap());
        let name = if kind == "GOOG1" {
            "x-goog-date"
        } else {
            "x-amz-date"
        };
        alternate.insert(name, "Tue, 27 Mar 2007 19:36:42 +0000".parse().unwrap());
        let prepared = prepare(&Method::GET, &uri, &alternate, now, &Resource::PathStyle).unwrap();
        assert_eq!(
            prepared.message,
            format!("GET\n\n\n\n{name}:Tue, 27 Mar 2007 19:36:42 +0000\n/bucket/object")
        );
    }
}

#[test]
fn legacy_credentials_and_prefixes_never_accept_mixed_duplicates_or_noncanonical_base64() {
    let now = parse_timestamp("20070327T193642Z").unwrap();
    for kind in ["GOOG1", "AWS"] {
        assert!(matches!(
            prepare(
                &Method::GET,
                &"/bucket/object?generation=123".parse().unwrap(),
                &headers(kind),
                now,
                &Resource::PathStyle
            ),
            Err(Failure::Malformed)
        ));
        for raw in [
            kind.to_string(),
            format!("{kind} :AA=="),
            format!("{kind} key:"),
            format!("{kind} key:YQ==:"),
            format!("{kind} key:YR=="),
        ] {
            let mut bad = headers(kind);
            bad.insert("authorization", raw.parse().unwrap());
            assert!(matches!(
                prepare(
                    &Method::GET,
                    &"/bucket/object".parse().unwrap(),
                    &bad,
                    now,
                    &Resource::PathStyle
                ),
                Err(Failure::Malformed)
            ));
        }
        let mut duplicate = headers(kind);
        duplicate.append("authorization", duplicate["authorization"].clone());
        assert!(matches!(
            prepare(
                &Method::GET,
                &"/bucket/object".parse().unwrap(),
                &duplicate,
                now,
                &Resource::PathStyle
            ),
            Err(Failure::Malformed)
        ));
        let mut mixed = headers(kind);
        mixed.insert(
            if kind == "GOOG1" {
                "x-amz-meta-a"
            } else {
                "x-goog-meta-a"
            },
            "mixed".parse().unwrap(),
        );
        assert!(matches!(
            prepare(
                &Method::GET,
                &"/bucket/object".parse().unwrap(),
                &mixed,
                now,
                &Resource::PathStyle
            ),
            Err(Failure::Malformed)
        ));
        assert!(matches!(
            prepare(
                &Method::GET,
                &"/bucket/object?GoogleAccessId=other&Expires=1&Signature=AA%3D%3D"
                    .parse()
                    .unwrap(),
                &headers(kind),
                now,
                &Resource::PathStyle
            ),
            Err(Failure::Malformed)
        ));
    }
}
