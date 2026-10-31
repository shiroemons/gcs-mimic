use super::*;
use crate::{
    fsutil::test_support::SourceDir,
    import::{self, MetadataMode, SourceFormat, SourcePlatform},
    storage::{
        Catalog,
        admin::{BucketName, Generation, ObjectName, Origin},
    },
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
};

#[test]
fn pinned_source_export_imports_selected_bodies_and_metadata_after_restart() {
    let (_dir, options) = fixture(Vec::new());
    let manifest = include_bytes!("../../../tests/fixtures/import/memory-snapshot.jsonl");
    fs::write(options.source.join("snapshot.jsonl"), manifest).unwrap();
    for (id, body) in [
        (1, b"old body".as_slice()),
        (2, b"new body".as_slice()),
        (3, b"retired body".as_slice()),
    ] {
        fs::write(
            options.source.join("blobs").join(format!("{id:016x}")),
            body,
        )
        .unwrap();
    }
    let summary = import::execute(options.clone(), Vec::new(), Vec::new()).unwrap();
    assert!(summary.complete && summary.original_names_verified && summary.bodies_verified);
    assert_eq!(
        (summary.buckets, summary.objects, summary.bytes_hashed),
        (2, 3, 28)
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for _ in 0..2 {
        runtime.block_on(async {
            let catalog = Catalog::open(&options.destination).await.unwrap();
            let admin = catalog.admin(Origin::Import);
            let bucket = BucketName::new("fixture-bucket").unwrap();
            for line in manifest
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                let row: Value = serde_json::from_slice(line).unwrap();
                if row["kind"] == "bucket" {
                    let source_time =
                        codec::parse_bucket_timestamp(row["created"].as_str().unwrap()).unwrap();
                    let settings = admin
                        .get_bucket(&BucketName::new(row["name"].as_str().unwrap()).unwrap())
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(settings.time_created, source_time);
                    assert_eq!(settings.updated, source_time);
                }
            }
            assert!(
                admin
                    .get_bucket(&BucketName::new("empty-bucket").unwrap())
                    .await
                    .unwrap()
                    .is_some()
            );
            for (name, generation, body, class, metadata) in [
                (
                    "a//../日本語/",
                    7,
                    b"new body".as_slice(),
                    "STANDARD",
                    Some("live"),
                ),
                (
                    "a//../日本語/",
                    900,
                    b"old body".as_slice(),
                    "NEARLINE",
                    Some("archived"),
                ),
                (
                    "archive-only",
                    11,
                    b"retired body".as_slice(),
                    "STANDARD",
                    None,
                ),
            ] {
                let name = ObjectName::new(name).unwrap();
                let selected = Some(Generation::new(generation).unwrap());
                let record = admin.get_object(&bucket, &name, selected).await.unwrap();
                assert_eq!(record.storage_class, class);
                assert_eq!(record.metadata.get("source").map(String::as_str), metadata);
                assert_eq!(record.time_deleted.is_some(), generation != 7);
                assert!(record.content_language.is_none());
                let mut file = admin
                    .open_object(&bucket, &name, selected)
                    .await
                    .unwrap()
                    .1
                    .into_file()
                    .await
                    .unwrap();
                let mut actual = Vec::new();
                file.read_to_end(&mut actual).unwrap();
                assert_eq!(actual, body);
            }
            assert_eq!(
                admin
                    .get_object(&bucket, &ObjectName::new("a//../日本語/").unwrap(), None)
                    .await
                    .unwrap()
                    .generation,
                7
            );
            assert!(
                admin
                    .get_object(&bucket, &ObjectName::new("archive-only").unwrap(), None)
                    .await
                    .is_err()
            );
            catalog.close().await.unwrap();
        });
    }
    assert_eq!(
        fs::read(options.source.join("snapshot.jsonl")).unwrap(),
        manifest
    );
}

fn object(name: &str, generation: i64, membership: &str) -> Value {
    let mut attributes: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/import/object-attrs.json"
    ))
    .unwrap();
    attributes["Generation"] = json!(generation);
    attributes["StorageClass"] = json!("NEARLINE");
    attributes["ACL"] = json!([{"Entity":"allUsers","Role":"READER"}]);
    if membership == "archived" {
        attributes["Deleted"] = json!("2026-01-02T03:04:05.123456+09:00");
    }
    json!({"kind":"object", "bucket":"bucket-one", "name":name, "membership":membership,
        "blob":"", "size":5, "attributes":attributes})
}

fn fixture(mut objects: Vec<Value>) -> (SourceDir, Options) {
    let dir = SourceDir::new();
    let source = dir.0.join("source");
    fs::create_dir_all(source.join("blobs")).unwrap();
    let mut options = Options::new(
        source,
        dir.0.join("destination"),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    options.source_format = SourceFormat::Snapshot;
    let header = json!({"kind":"snapshot","version":1,"source_revision":super::super::SOURCE_REVISION,
        "capture_id":"0123456789abcdef0123456789abcdef","quiesced":true,"backend":"memory","original_names_verified":true});
    let bucket = json!({"kind":"bucket","name":"bucket-one","created":"2026-01-01T00:00:00.123456789Z",
        "attributes":{"VersioningEnabled":true,"DefaultEventBasedHold":true,
            "ACL":[{"Entity":"allAuthenticatedUsers","Role":"READER"}]},"enable_object_retention":true});
    let mut bytes = Vec::new();
    for row in [header, bucket] {
        serde_json::to_writer(&mut bytes, &row).unwrap();
        bytes.push(b'\n');
    }
    for (index, row) in objects.iter_mut().enumerate() {
        let blob = format!("{:016x}", index + 1);
        row["blob"] = json!(blob);
        fs::write(options.source.join("blobs").join(blob), b"hello").unwrap();
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    let trailer = json!({"kind":"complete","buckets":1,"generations":objects.len(),"bytes":objects.len()*5,
        "manifest_sha256":encode_lower(ring::digest::digest(&SHA256,&bytes).as_ref())});
    serde_json::to_writer(&mut bytes, &trailer).unwrap();
    bytes.push(b'\n');
    fs::write(options.source.join("snapshot.jsonl"), bytes).unwrap();
    (dir, options)
}

struct Interrupt(usize);
impl Write for Interrupt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.contains(&b'\n') {
            self.0 -= 1;
            if self.0 == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn snapshot_preserves_original_names_history_and_archive_only_after_resume_and_restart() {
    let name = "a//../é/";
    let (_dir, options) = fixture(vec![
        object(name, 9_000_000_000_000_000, "archived"),
        object(name, 7, "live"),
        object("archive-only", 11, "archived"),
    ]);
    let manifest = fs::read(options.source.join("snapshot.jsonl")).unwrap();
    let before =
        Fingerprint::of(&fs::metadata(options.source.join("snapshot.jsonl")).unwrap()).unwrap();
    assert!(import::execute(options.clone(), Interrupt(3), Vec::new()).is_err());
    let summary = import::execute(options.clone(), Vec::new(), Vec::new()).unwrap();
    assert!(summary.complete && summary.bodies_verified);
    assert_eq!(summary.objects, 3);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for _ in 0..2 {
        runtime.block_on(async {
            let catalog = Catalog::open(&options.destination).await.unwrap();
            let admin = catalog.admin(Origin::Import);
            let bucket = BucketName::new("bucket-one").unwrap();
            let live = admin
                .get_object(&bucket, &ObjectName::new(name).unwrap(), None)
                .await
                .unwrap();
            assert_eq!(live.generation, 7);
            assert_eq!(live.storage_class, "NEARLINE");
            assert!(!live.event_based_hold);
            let archived = admin
                .get_object(
                    &bucket,
                    &ObjectName::new(name).unwrap(),
                    Some(Generation::new(9_000_000_000_000_000).unwrap()),
                )
                .await
                .unwrap();
            assert_eq!(
                archived.time_deleted,
                Some(
                    super::super::filesystem::time(
                        codec::parse_timestamp("2026-01-02T03:04:05.123456+09:00").unwrap()
                    )
                    .unwrap()
                )
            );
            let missing = ObjectName::new("archive-only").unwrap();
            assert!(admin.get_object(&bucket, &missing, None).await.is_err());
            assert!(
                admin
                    .get_object(&bucket, &missing, Some(Generation::new(11).unwrap()))
                    .await
                    .unwrap()
                    .time_deleted
                    .is_some()
            );
            let settings = admin.get_bucket(&bucket).await.unwrap().unwrap();
            assert!(
                settings.versioning_enabled
                    && settings.object_retention_enabled
                    && settings.default_event_based_hold
            );
            assert_eq!(
                settings.time_created,
                codec::parse_bucket_timestamp("2026-01-01T00:00:00.123456789Z").unwrap()
            );
            let resource = crate::json::bucket_resource(
                &settings,
                "http://localhost",
                crate::json::Projection::NoAcl,
            )
            .unwrap();
            assert_eq!(resource["timeCreated"], "2026-01-01T00:00:00.123456789Z");
            assert_eq!(resource["updated"], "2026-01-01T00:00:00.123456789Z");
            let timestamp: prost_types::Timestamp = settings.time_created.into();
            assert_eq!(timestamp.nanos, 123_456_789);
            catalog.close().await.unwrap();
        });
    }
    runtime.block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let before = catalog.get_bucket("bucket-one").await.unwrap().unwrap();
        let patched = catalog
            .patch_bucket(
                "bucket-one",
                crate::bucket::BucketMetadataUpdate::default(),
                None,
                None,
                &crate::bucket::BucketAccess::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(patched.time_created, before.time_created);
        assert_eq!(
            patched
                .updated
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
                % 1000,
            0
        );
        assert_eq!(
            catalog.get_bucket("bucket-one").await.unwrap().unwrap(),
            patched
        );
        let next = catalog
            .admin(Origin::Api)
            .publish_from_file(
                &BucketName::new("bucket-one").unwrap(),
                &ObjectName::new("new-api").unwrap(),
                crate::storage::admin::Publication {
                    content_type: "text/plain".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: Some(crate::storage::admin::GenerationCondition::Absent),
                    max_bytes: 1024,
                    mode: crate::storage::admin::PublishMode::Replace,
                },
                File::open(options.source.join("blobs/0000000000000001")).unwrap(),
            )
            .await
            .unwrap();
        assert!(next.generation > 9_000_000_000_000_000);
        catalog.close().await.unwrap();
    });
    assert_eq!(
        fs::read(options.source.join("snapshot.jsonl")).unwrap(),
        manifest
    );
    assert_eq!(
        Fingerprint::of(&fs::metadata(options.source.join("snapshot.jsonl")).unwrap()).unwrap(),
        before
    );
}

#[test]
fn snapshot_rejects_invalid_membership_duplicate_generations_and_incomplete_exports_before_destination()
 {
    for rows in [
        vec![object("same", 7, "live"), object("same", 8, "live")],
        vec![object("same", 7, "archived"), object("same", 7, "live")],
        vec![object("z", 7, "archived"), object("a", 8, "live")],
    ] {
        let (_dir, options) = fixture(rows);
        assert!(import::execute(options.clone(), Vec::new(), Vec::new()).is_err());
        assert!(!options.destination.exists());
    }
    for corruption in ["trailer", "extra", "missing", "checksum", "digest"] {
        let (_dir, options) = fixture(vec![object("body", 7, "live")]);
        let manifest = options.source.join("snapshot.jsonl");
        match corruption {
            "trailer" => {
                let mut bytes = fs::read(&manifest).unwrap();
                bytes.truncate(bytes.len() - 2);
                fs::write(&manifest, bytes).unwrap();
            }
            "extra" => fs::write(options.source.join("blobs/unreferenced"), b"unused").unwrap(),
            "missing" => fs::remove_file(options.source.join("blobs/0000000000000001")).unwrap(),
            "checksum" => {
                fs::write(options.source.join("blobs/0000000000000001"), b"world").unwrap()
            }
            "digest" => {
                let mut bytes = fs::read(&manifest).unwrap();
                bytes[30] = b'x';
                fs::write(&manifest, bytes).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            import::execute(options.clone(), Vec::new(), Vec::new()).is_err(),
            "{corruption}"
        );
        if corruption != "checksum" {
            assert!(!options.destination.exists(), "{corruption}");
        }
    }
}

#[test]
fn snapshot_retention_remains_enforced_after_restart() {
    let mut source = object("retained", 7, "live");
    source["attributes"]["RetentionMode"] = json!("Locked");
    source["attributes"]["RetentionRetainUntil"] = json!("2099-01-01T00:00:00.123456Z");
    let (_dir, options) = fixture(vec![source]);
    assert!(
        import::execute(options.clone(), Vec::new(), Vec::new())
            .unwrap()
            .complete
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let admin = catalog.admin(Origin::Import);
        let bucket = BucketName::new("bucket-one").unwrap();
        let name = ObjectName::new("retained").unwrap();
        let record = admin.get_object(&bucket, &name, None).await.unwrap();
        assert_eq!(
            record.retention.unwrap().mode,
            crate::object::retention::RetentionMode::Locked
        );
        assert_eq!(
            catalog
                .delete_object("bucket-one", "retained", Some(7), Default::default())
                .await,
            Err(crate::object::ObjectError::ObjectRetentionNotMet)
        );
        assert_eq!(
            admin.get_object(&bucket, &name, None).await.unwrap(),
            record
        );
        catalog.close().await.unwrap();
    });
}

#[test]
fn snapshot_source_attribute_cap_is_separate_from_wrapper_and_cli_selects_format() {
    for excess in [0, 1] {
        let mut source = object("body", 7, "live");
        source["attributes"]["Metadata"] = json!({"large":""});
        let overhead = serde_json::to_vec(&source["attributes"]).unwrap().len();
        source["attributes"]["Metadata"]["large"] =
            json!("x".repeat(fsutil::SOURCE_METADATA_CAP - overhead + excess));
        let (_dir, options) = fixture(vec![source]);
        let mut output = Vec::new();
        let summary = import::inspect(options.clone(), &mut output, Vec::new()).unwrap();
        assert_eq!(summary.eligible, excess == 0);
        assert!(!options.destination.exists());
        if excess == 0 {
            let header: Value =
                serde_json::from_slice(output.split(|byte| *byte == b'\n').next().unwrap())
                    .unwrap();
            assert_eq!(header["original_names_verified"], false);
            assert!(summary.original_names_verified);
            let (_small_dir, options) = fixture(vec![object("body", 7, "live")]);
            let mut args = vec![
                "--source".into(),
                options.source.to_string_lossy().into_owned(),
                "--data-dir".into(),
                options.destination.to_string_lossy().into_owned(),
                "--source-format".into(),
                "snapshot".into(),
                "--dry-run".into(),
            ];
            assert!(import::cli(&args).is_ok());
            args.extend(["--metadata-mode".into(), "sidecar".into()]);
            assert!(import::cli(&args).is_err());
        }
    }
}

#[test]
fn snapshot_preserves_empty_memory_buckets() {
    let (_dir, options) = fixture(Vec::new());
    let result = import::execute(options, Vec::new(), Vec::new()).unwrap();
    assert!(result.complete);
    assert_eq!((result.buckets, result.objects), (1, 0));
}
