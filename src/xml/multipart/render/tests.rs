use super::*;
use crate::xml::test_support::object_record;

fn at() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_789)
}

fn target(name: &str, id: &str) -> storage::Target {
    storage::Target {
        bucket: "bucket".into(),
        name: name.into(),
        id: id.into(),
    }
}

fn upload(name: &str, id: &str) -> storage::Upload {
    let mut options = crate::storage::ResumableOptions::new(BucketAccess::Permissive);
    options.metadata.storage_class = Some(crate::bucket::StorageClass::Coldline);
    storage::Upload::for_test(target(name, id), options, at())
}

/// The expected text is the current output, written out rather than derived.
#[test]
fn multipart_initiate_complete_parts_and_uploads_documents() {
    assert_eq!(
        initiate_body(&target("a&b", "upload-1")),
        "<?xml version='1.0' encoding='UTF-8'?><InitiateMultipartUploadResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><Key>a&amp;b</Key><UploadId>upload-1</UploadId></InitiateMultipartUploadResult>"
    );
    assert_eq!(
        complete_body(
            "http://localhost:4443/",
            &object_record("bucket", "dir/a b", at())
        ),
        "<?xml version='1.0' encoding='UTF-8'?><CompleteMultipartUploadResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Location>http://localhost:4443/bucket/dir/a%20b</Location><Bucket>bucket</Bucket><Key>dir/a b</Key><ETag>\"03030303030303030303030303030303\"</ETag></CompleteMultipartUploadResult>"
    );
    let parts = [
        storage::Part::for_test(1, "\"etag-1\"", 5, at()),
        storage::Part::for_test(2, "\"etag-2\"", 6, at()),
    ];
    assert_eq!(
        parts_body(&upload("a", "upload-1"), &parts, 0, 2, true).unwrap(),
        "<?xml version='1.0' encoding='UTF-8'?><ListPartsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><Key>a</Key><UploadId>upload-1</UploadId><StorageClass>COLDLINE</StorageClass><PartNumberMarker>0</PartNumberMarker><NextPartNumberMarker>2</NextPartNumberMarker><MaxParts>2</MaxParts><IsTruncated>true</IsTruncated><Part><PartNumber>1</PartNumber><LastModified>2023-11-14T22:13:20.123456Z</LastModified><ETag>\"etag-1\"</ETag><Size>5</Size></Part><Part><PartNumber>2</PartNumber><LastModified>2023-11-14T22:13:20.123456Z</LastModified><ETag>\"etag-2\"</ETag><Size>6</Size></Part></ListPartsResult>"
    );
    let page = storage::UploadPage {
        uploads: vec![upload("p/b", "upload-1")],
        prefixes: vec!["p/c/".into()],
        next_key: "p/c/".into(),
        next_upload: String::new(),
        more: true,
    };
    assert_eq!(
        uploads_body(&request("p/", "/"), page).unwrap(),
        "<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><KeyMarker>p/a</KeyMarker><UploadIdMarker>upload-0</UploadIdMarker><NextKeyMarker>p/c/</NextKeyMarker><Prefix>p/</Prefix><Delimiter>/</Delimiter><NextUploadIdMarker></NextUploadIdMarker><MaxUploads>2</MaxUploads><IsTruncated>true</IsTruncated><Upload><Key>p/b</Key><UploadId>upload-1</UploadId><StorageClass>COLDLINE</StorageClass><Initiated>2023-11-14T22:13:20.123456Z</Initiated></Upload><CommonPrefixes><Prefix>p/c/</Prefix></CommonPrefixes></ListMultipartUploadsResult>"
    );
}

fn request(prefix: &str, delimiter: &str) -> UploadsRequest {
    UploadsRequest {
        bucket: "bucket".into(),
        prefix: Some(prefix.into()),
        delimiter: Some(delimiter.into()),
        key_marker: "p/a".into(),
        upload_marker: Some("upload-0".into()),
        limit: 2,
        encoding: NameEncoding::Plain,
    }
}

fn last_page(uploads: Vec<storage::Upload>) -> storage::UploadPage {
    storage::UploadPage {
        uploads,
        prefixes: Vec::new(),
        // The catalog's cursor after the last entry; the last page does not show it.
        next_key: "f g".into(),
        next_upload: "upload-2".into(),
        more: false,
    }
}

/// Recorded 2026-11-16: next markers present and empty on the last page, `Prefix` and
/// `Delimiter` only when sent, `EncodingType` last, and a URL-encoded `Key` with `+` for a space.
#[test]
fn upload_listings_echo_only_what_was_sent_and_encode_names_on_request() {
    let unfiltered = UploadsRequest {
        prefix: None,
        delimiter: None,
        key_marker: String::new(),
        upload_marker: None,
        ..request("", "")
    };
    assert_eq!(
        uploads_body(&unfiltered, last_page(vec![upload("f g", "upload-2")])).unwrap(),
        "<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><KeyMarker></KeyMarker><UploadIdMarker></UploadIdMarker><NextKeyMarker></NextKeyMarker><NextUploadIdMarker></NextUploadIdMarker><MaxUploads>2</MaxUploads><IsTruncated>false</IsTruncated><Upload><Key>f g</Key><UploadId>upload-2</UploadId><StorageClass>COLDLINE</StorageClass><Initiated>2023-11-14T22:13:20.123456Z</Initiated></Upload></ListMultipartUploadsResult>"
    );
    let encoded = UploadsRequest {
        encoding: NameEncoding::Form,
        ..unfiltered
    };
    let mut page = last_page(vec![upload("f g", "upload-2")]);
    page.prefixes.push("d/".into());
    assert_eq!(
        uploads_body(&encoded, page).unwrap(),
        "<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><KeyMarker></KeyMarker><UploadIdMarker></UploadIdMarker><NextKeyMarker></NextKeyMarker><NextUploadIdMarker></NextUploadIdMarker><MaxUploads>2</MaxUploads><IsTruncated>false</IsTruncated><Upload><Key>f+g</Key><UploadId>upload-2</UploadId><StorageClass>COLDLINE</StorageClass><Initiated>2023-11-14T22:13:20.123456Z</Initiated></Upload><CommonPrefixes><Prefix>d/</Prefix></CommonPrefixes><EncodingType>url</EncodingType></ListMultipartUploadsResult>"
    );
}

#[test]
fn an_upload_listing_request_ignores_a_lone_upload_id_marker_and_names_a_bad_encoding() {
    let parse = |pairs: &[(&str, &str)]| {
        let mut query: BTreeMap<String, String> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        UploadsRequest::parse("bucket".into(), &mut query)
    };
    let lone = parse(&[("upload-id-marker", "upload-0")]).unwrap();
    assert_eq!(lone.upload_marker, None);
    let paired = parse(&[("key-marker", "a"), ("upload-id-marker", "upload-0")]).unwrap();
    assert_eq!(paired.upload_marker.as_deref(), Some("upload-0"));
    let sent_empty = parse(&[("prefix", "")]).unwrap();
    assert_eq!(
        (sent_empty.prefix.as_deref(), sent_empty.delimiter),
        (Some(""), None)
    );
    assert_eq!(
        parse(&[("encoding-type", "url")]).unwrap().encoding,
        NameEncoding::Form
    );
    let error = parse(&[("encoding-type", "bogus")]).err().unwrap();
    assert_eq!(
        error.details.as_deref(),
        Some("Invalid encoding type: bogus")
    );
}

#[test]
fn a_completion_location_is_http_whatever_the_advertised_scheme() {
    let record = object_record("bucket", "e.bin", at());
    let body = complete_body("https://storage.example/", &record);
    assert!(
        body.contains("<Location>http://storage.example/bucket/e.bin</Location>"),
        "{body}"
    );
}
