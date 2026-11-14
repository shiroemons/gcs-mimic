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
        "<?xml version='1.0' encoding='UTF-8'?><ListPartsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><Key>a</Key><UploadId>upload-1</UploadId><StorageClass>COLDLINE</StorageClass><PartNumberMarker>0</PartNumberMarker><MaxParts>2</MaxParts><IsTruncated>true</IsTruncated><NextPartNumberMarker>2</NextPartNumberMarker><Part><PartNumber>1</PartNumber><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"etag-1\"</ETag><Size>5</Size></Part><Part><PartNumber>2</PartNumber><LastModified>2023-11-14T22:13:20.123Z</LastModified><ETag>\"etag-2\"</ETag><Size>6</Size></Part></ListPartsResult>"
    );
    let query = storage::UploadQuery {
        bucket: "bucket".into(),
        prefix: "p/".into(),
        delimiter: "/".into(),
        key_marker: "p/a".into(),
        upload_marker: Some("upload-0".into()),
        limit: 2,
    };
    let page = storage::UploadPage {
        uploads: vec![upload("p/b", "upload-1")],
        prefixes: vec!["p/c/".into()],
        next_key: "p/c/".into(),
        next_upload: String::new(),
        more: true,
    };
    assert_eq!(
        uploads_body(&query, page).unwrap(),
        "<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult xmlns='http://s3.amazonaws.com/doc/2006-03-01/'><Bucket>bucket</Bucket><KeyMarker>p/a</KeyMarker><UploadIdMarker>upload-0</UploadIdMarker><Prefix>p/</Prefix><Delimiter>/</Delimiter><MaxUploads>2</MaxUploads><IsTruncated>true</IsTruncated><NextKeyMarker>p/c/</NextKeyMarker><NextUploadIdMarker></NextUploadIdMarker><Upload><Key>p/b</Key><UploadId>upload-1</UploadId><StorageClass>COLDLINE</StorageClass><Initiated>2023-11-14T22:13:20.123Z</Initiated></Upload><CommonPrefixes><Prefix>p/c/</Prefix></CommonPrefixes></ListMultipartUploadsResult>"
    );
}
