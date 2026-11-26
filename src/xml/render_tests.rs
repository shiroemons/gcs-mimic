//! The bytes of every XML response document. The expected text is the current output, written
//! out rather than derived, so a change to how documents are assembled cannot move a byte.

use super::*;
use crate::server::ResponseBody;
use crate::storage::{ObjectListCursor, ObjectListPage};
use test_support::object_record;

fn at() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_789)
}

async fn body_text(response: Response<ResponseBody>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn listing(query: &str, headers: &HeaderMap, page: &ObjectListPage) -> String {
    let params = list::parse("bucket".into(), query, headers).unwrap();
    list::render(&params, page).unwrap()
}

#[test]
fn xml_condition_grammars_agree_and_keep_both_error_messages() {
    for (messages, repeated, invalid) in [
        (
            ConditionMessages::Object,
            "Multiple precondition header values.",
            "Invalid precondition header.",
        ),
        (
            ConditionMessages::Upload,
            "Expected one header value.",
            "Header must be representable as ASCII.",
        ),
    ] {
        let generation = "x-goog-if-generation-match";
        let metageneration = "x-goog-if-metageneration-match";
        let mut headers = HeaderMap::new();
        headers.insert(generation, HeaderValue::from_static("0"));
        headers.insert(
            metageneration,
            HeaderValue::from_static("9223372036854775807"),
        );
        let parsed = conditions(&headers, messages).unwrap();
        assert_eq!(parsed.if_generation_match, Some(0));
        assert_eq!(parsed.if_metageneration_match, Some(i64::MAX));

        for (name, input) in [
            (generation, "-1"),
            (generation, "+1"),
            (metageneration, "0"),
            (metageneration, "9223372036854775808"),
        ] {
            let mut bad = headers.clone();
            bad.insert(name, HeaderValue::from_static(input));
            let error = conditions(&bad, messages).unwrap_err();
            assert_eq!(error.code, XmlCode::InvalidArgument);
            assert_eq!(error.message, "Invalid generation or metageneration.");
        }
        headers.insert(generation, HeaderValue::from_bytes(&[0xff]).unwrap());
        let error = conditions(&headers, messages).unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (XmlCode::InvalidArgument, invalid)
        );
        headers.append(generation, HeaderValue::from_static("0"));
        let error = conditions(&headers, messages).unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (XmlCode::ExcessHeaderValues, repeated)
        );
    }
}

#[test]
fn concrete_generation_requires_positive_digit_text_within_int64() {
    assert_eq!(
        number::<Generation>("09223372036854775807").unwrap().get(),
        i64::MAX
    );
    for text in ["0", "-1", "+1", "9223372036854775808"] {
        let error = number::<Generation>(text).unwrap_err();
        assert_eq!(error.code, XmlCode::InvalidArgument);
        assert_eq!(error.message, "Invalid generation or metageneration.");
    }
}

#[test]
fn xml_object_and_completion_etags_choose_the_same_validator() {
    let mut record = object_record("bucket", "object", at());
    let revision = crate::etag::object_etag(&record);
    let md5 = crate::base64::encode(&record.md5);
    let plain = serde_json::json!({});
    let encrypted = serde_json::json!({"customerEncryption": {}});
    let composite = serde_json::json!({"componentCount": 2});
    let etag = |md5, value| {
        Validators::for_completion(md5, value)
            .unwrap()
            .etag(|| &revision)
    };
    assert_eq!(
        etag(Some(&md5), &plain),
        "\"03030303030303030303030303030303\""
    );
    assert_eq!(object_etag(&record), etag(Some(&md5), &plain));
    assert!(Validators::for_completion(Some("bad"), &plain).is_err());

    record.customer_key_sha256 = Some([1; 32]);
    assert_eq!(object_etag(&record), etag(Some("bad"), &encrypted));
    record.customer_key_sha256 = None;
    record.component_count = Some(2);
    assert_eq!(object_etag(&record), etag(Some("bad"), &composite));
    record.component_count = None;
    record.md5_availability = crate::object::Md5Availability::Unavailable;
    assert_eq!(object_etag(&record), etag(None, &plain));
    assert_eq!(object_etag(&record), format!("\"{revision}\""));
}

/// Cloud Storage writes `<ETag>"…"</ETag>`, so text elements escape `&`, `<` and `>` only.
#[tokio::test]
async fn error_document_keeps_quotes_literal() {
    let response = Error::new(XmlCode::InvalidArgument, "a \"quoted\" <name> & 'more'")
        .response(ErrorBody::Include);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        String::from_utf8(bytes.to_vec()).unwrap(),
        "<?xml version='1.0' encoding='UTF-8'?><Error><Code>InvalidArgument</Code><Message>a \"quoted\" &lt;name&gt; &amp; 'more'</Message></Error>"
    );
}

#[test]
fn v1_listing_with_versions_markers_delimiter_and_url_encoding() {
    let page = ObjectListPage {
        items: vec![object_record("bucket", "p/a b&c<d>", at())],
        prefixes: vec!["p/dir/".into()],
        next: Some(ObjectListCursor {
            name: "p/z".into(),
            is_prefix: false,
            generation: Some(43),
        }),
        bucket_metageneration: None,
    };
    assert_eq!(
        listing(
            "versions&marker=p%2Fa&generation-marker=7&delimiter=%2F&encoding-type=url&max-keys=2&prefix=p%2F",
            &HeaderMap::new(),
            &page
        ),
        "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix>p%2F</Prefix><Marker>p%2Fa</Marker><GenerationMarker>7</GenerationMarker><Delimiter>%2F</Delimiter><NextMarker>p%2Fz</NextMarker><NextGenerationMarker>43</NextGenerationMarker><MaxKeys>2</MaxKeys><IsTruncated>true</IsTruncated><EncodingType>url</EncodingType><Version><Key>p%2Fa%20b%26c%3Cd%3E</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><IsLatest>true</IsLatest><LastModified>2023-11-14T22:13:20.123Z</LastModified><DeletedTime></DeletedTime><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size></Version><CommonPrefixes><Prefix>p%2Fdir%2F</Prefix></CommonPrefixes></ListBucketResult>"
    );
}

#[test]
fn v1_interoperable_versions_listing() {
    let mut deleted = object_record("bucket", "a", at());
    deleted.time_deleted = Some(at() + std::time::Duration::from_secs(1));
    let page = ObjectListPage {
        items: vec![deleted],
        prefixes: Vec::new(),
        next: None,
        bucket_metageneration: None,
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-interop-list-objects-format",
        "enabled".parse().unwrap(),
    );
    assert_eq!(
        listing("versions&key-marker=a&version-id-marker=5", &headers, &page),
        "<?xml version='1.0' encoding='UTF-8'?><ListVersionsResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix></Prefix><KeyMarker>a</KeyMarker><VersionIdMarker>5</VersionIdMarker><IsTruncated>false</IsTruncated><Version><Key>a</Key><VersionId>42</VersionId><MetaGeneration>2</MetaGeneration><IsLatest>false</IsLatest><LastModified>2023-11-14T22:13:20.123Z</LastModified><DeletedTime>2023-11-14T22:13:21.123Z</DeletedTime><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size><StorageClass>NEARLINE</StorageClass></Version></ListVersionsResult>"
    );
}

/// Recorded version 2 shape: the request echoes and the next token before KeyCount, which
/// counts `Contents` only; MaxKeys only when requested; Delimiter after them; no StorageClass.
#[test]
fn v2_listing_with_tokens_and_start_after() {
    let page = ObjectListPage {
        items: vec![object_record("bucket", "p/b", at())],
        prefixes: vec!["p/c/".into()],
        next: Some(ObjectListCursor {
            name: "p/c/".into(),
            is_prefix: true,
            generation: None,
        }),
        bucket_metageneration: None,
    };
    let token = crate::page_token::encode_object_page_token(&ObjectListCursor {
        name: "p/a".into(),
        is_prefix: false,
        generation: None,
    });
    assert_eq!(
        listing(
            "list-type=2&prefix=p%2F&delimiter=%2F&start-after=p%2Fa",
            &HeaderMap::new(),
            &page
        ),
        "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix>p/</Prefix><StartAfter>p/a</StartAfter><NextContinuationToken>xml2.5b226f33222c22702f632f222c747275652c6e756c6c5d.cc6e8f7415f00927</NextContinuationToken><KeyCount>1</KeyCount><Delimiter>/</Delimiter><IsTruncated>true</IsTruncated><Contents><Key>p/b</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size></Contents><CommonPrefixes><Prefix>p/c/</Prefix></CommonPrefixes></ListBucketResult>"
    );
    assert_eq!(
        listing(
            &format!(
                "list-type=2&prefix=p%2F&delimiter=%2F&start-after=p%2F0&continuation-token=xml2.{token}&max-keys=5"
            ),
            &HeaderMap::new(),
            &page
        ),
        "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix>p/</Prefix><StartAfter>p/0</StartAfter><ContinuationToken>xml2.5b226f33222c22702f61222c66616c73652c6e756c6c5d.e7f3bde90b7ce12b</ContinuationToken><NextContinuationToken>xml2.5b226f33222c22702f632f222c747275652c6e756c6c5d.cc6e8f7415f00927</NextContinuationToken><KeyCount>1</KeyCount><MaxKeys>5</MaxKeys><Delimiter>/</Delimiter><IsTruncated>true</IsTruncated><Contents><Key>p/b</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size></Contents><CommonPrefixes><Prefix>p/c/</Prefix></CommonPrefixes></ListBucketResult>"
    );
}

/// Cloud Storage echoed an empty `continuation-token` or `start-after` as an empty element, the
/// start before the token, and wrote neither when the request did not send it (recorded).
#[test]
fn v2_listing_echoes_empty_tokens_and_start_after() {
    let page = ObjectListPage {
        items: vec![object_record("bucket", "b", at())],
        prefixes: Vec::new(),
        next: None,
        bucket_metageneration: None,
    };
    let contents = "<Contents><Key>b</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size></Contents>";
    for (query, echoes) in [
        ("list-type=2", ""),
        (
            "list-type=2&continuation-token=",
            "<ContinuationToken></ContinuationToken>",
        ),
        ("list-type=2&start-after=", "<StartAfter></StartAfter>"),
        (
            "list-type=2&continuation-token=&start-after=a",
            "<StartAfter>a</StartAfter><ContinuationToken></ContinuationToken>",
        ),
    ] {
        assert_eq!(
            listing(query, &HeaderMap::new(), &page),
            format!(
                "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix></Prefix>{echoes}<KeyCount>1</KeyCount><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
            ),
            "{query}"
        );
    }
}

/// With `encoding-type=url`, version 2 wrote a space as `+` and kept `/` (recorded), in keys,
/// common prefixes and the echoed names alike.
#[test]
fn v2_listing_url_encodes_names_form_style() {
    let page = ObjectListPage {
        items: vec![object_record("bucket", "d/f g+h", at())],
        prefixes: vec!["d/x y/".into()],
        next: None,
        bucket_metageneration: None,
    };
    assert_eq!(
        listing(
            "list-type=2&encoding-type=url&prefix=d%2F&delimiter=%2F&start-after=d%2Fc%20e",
            &HeaderMap::new(),
            &page
        ),
        "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix>d/</Prefix><StartAfter>d/c+e</StartAfter><KeyCount>1</KeyCount><Delimiter>/</Delimiter><IsTruncated>false</IsTruncated><EncodingType>url</EncodingType><Contents><Key>d/f+g%2Bh</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size></Contents><CommonPrefixes><Prefix>d/x+y/</Prefix></CommonPrefixes></ListBucketResult>"
    );
}

/// Cloud Storage answered `continuation-token=bogus` with this document (recorded). A JSON page
/// token and a corrupted XML token are refused the same way.
#[tokio::test]
async fn v2_listing_refuses_a_token_it_did_not_issue() {
    let json_token = crate::page_token::encode_object_page_token(&ObjectListCursor {
        name: "a".into(),
        is_prefix: false,
        generation: None,
    });
    for token in ["bogus", "xml2.", "xml2.bogus", json_token.as_str()] {
        let error = list::parse(
            "bucket".into(),
            &format!("list-type=2&continuation-token={token}"),
            &HeaderMap::new(),
        )
        .err()
        .expect("the token is refused");
        let response = error.response(ErrorBody::Include);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{token}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            "<?xml version='1.0' encoding='UTF-8'?><Error><Code>InvalidArgument</Code><Message>Invalid argument.</Message><Details>Invalid page token.</Details></Error>",
            "{token}"
        );
    }
}

#[tokio::test]
async fn copy_result_document() {
    let record = object_record("bucket", "copied", at());
    let response = copy::response(&record, at()).unwrap();
    assert_eq!(
        body_text(response).await,
        "<?xml version='1.0' encoding='UTF-8'?><CopyObjectResult><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag></CopyObjectResult>"
    );
}

/// Unlike other documents, the POST response escapes quotes and apostrophes too.
#[tokio::test]
async fn post_response_escapes_quotes() {
    let record = object_record("bucket", "a\"b'c<d>&e", at());
    let response =
        post::success_response(post::Success::Created, &record, "http://localhost:4443/").unwrap();
    assert_eq!(
        body_text(response).await,
        "<?xml version='1.0' encoding='UTF-8'?><PostResponse><Location>http://localhost:4443/bucket/a%22b%27c%3Cd%3E%26e</Location><Bucket>bucket</Bucket><Key>a&quot;b&apos;c&lt;d&gt;&amp;e</Key><ETag>&quot;03030303030303030303030303030303&quot;</ETag></PostResponse>"
    );
}
