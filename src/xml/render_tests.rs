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
    let token = json::encode_object_page_token(&ObjectListCursor {
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
        "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix>p/</Prefix><Delimiter>/</Delimiter><MaxKeys>1000</MaxKeys><IsTruncated>true</IsTruncated><KeyCount>2</KeyCount><StartAfter>p/a</StartAfter><NextContinuationToken>xml2.5b226f33222c22702f632f222c747275652c6e756c6c5d.cc6e8f7415f00927</NextContinuationToken><Contents><Key>p/b</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size><StorageClass>NEARLINE</StorageClass></Contents><CommonPrefixes><Prefix>p/c/</Prefix></CommonPrefixes></ListBucketResult>"
    );
    assert_eq!(
        listing(
            &format!("list-type=2&prefix=p%2F&continuation-token=xml2.{token}&max-keys=5"),
            &HeaderMap::new(),
            &page
        ),
        "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name>bucket</Name><Prefix>p/</Prefix><MaxKeys>5</MaxKeys><IsTruncated>true</IsTruncated><KeyCount>2</KeyCount><ContinuationToken>xml2.5b226f33222c22702f61222c66616c73652c6e756c6c5d.e7f3bde90b7ce12b</ContinuationToken><NextContinuationToken>xml2.5b226f33222c22702f632f222c747275652c6e756c6c5d.cc6e8f7415f00927</NextContinuationToken><Contents><Key>p/b</Key><Generation>42</Generation><MetaGeneration>2</MetaGeneration><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"03030303030303030303030303030303\"</ETag><Size>5</Size><StorageClass>NEARLINE</StorageClass></Contents><CommonPrefixes><Prefix>p/c/</Prefix></CommonPrefixes></ListBucketResult>"
    );
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
    let response = post::success_response(
        post::Success::Status(StatusCode::CREATED),
        &record,
        "http://localhost:4443/",
    )
    .unwrap();
    assert_eq!(
        body_text(response).await,
        "<?xml version='1.0' encoding='UTF-8'?><PostResponse><Location>http://localhost:4443/bucket/a%22b%27c%3Cd%3E%26e</Location><Bucket>bucket</Bucket><Key>a&quot;b&apos;c&lt;d&gt;&amp;e</Key><ETag>&quot;03030303030303030303030303030303&quot;</ETag></PostResponse>"
    );
}
