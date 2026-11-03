use gcs_mimic::storage::admin::{BucketName, ObjectName};
use gcs_mimic::sync::mapping::{PathMapping, name_to_path, path_to_name};

#[test]
fn mapping_preserves_the_api_name_byte_limit() {
    let bucket = BucketName::new("demo-bucket").unwrap();
    let object = ObjectName::new("é".repeat(512)).unwrap();
    let PathMapping::File(path) = name_to_path(&bucket, &object) else {
        panic!("valid API-sized lexical candidate");
    };
    assert_eq!(path_to_name(&path), Ok((bucket, object)));
    for object in [
        "é".repeat(513),
        "x".repeat(1025),
        "a\nb".into(),
        "a\rb".into(),
    ] {
        assert!(ObjectName::new(object).is_err());
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_rejected_without_lossy_conversion() {
    use gcs_mimic::sync::mapping::Reason;
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};
    let path = Path::new(OsStr::from_bytes(b"demo-bucket/\xff"));
    assert_eq!(path_to_name(path), Err(Reason::InvalidName));
}
