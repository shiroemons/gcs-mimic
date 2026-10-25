//! Local administrative fixture import, completed before the listener binds.

use crate::{
    clock::Clock,
    config::Config,
    json::validate_object_name,
    object::{ExpectedHashes, UploadChunk},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BucketDefinition, BucketName, EnsureBucket, GenerationCondition,
            ObjectName, Origin, Publication, PublishMode,
        },
    },
};
use bytes::Bytes;
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::{io::AsyncReadExt, sync::mpsc};

const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum SeedOutcome {
    Disabled,
    Skipped,
    Imported {
        buckets: usize,
        objects: usize,
        existing_objects: usize,
    },
}

pub async fn import(
    catalog: &Catalog,
    config: &Config,
    clock: &dyn Clock,
) -> io::Result<SeedOutcome> {
    let admin = catalog.admin(Origin::Seed);
    if admin.seed_complete().await.map_err(io::Error::other)? {
        return Ok(SeedOutcome::Skipped);
    }
    let Some(root) = &config.seed_dir else {
        admin.finish_seed().await.map_err(io::Error::other)?;
        return Ok(SeedOutcome::Disabled);
    };
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        "seeding",
        serde_json::json!({}),
    );
    let result = import_directory(&admin, config, clock, root).await;
    result.map_err(|error| io::Error::other(format!("seed import: {error}")))
}

async fn entries(path: &Path, count: &mut usize) -> io::Result<Vec<PathBuf>> {
    let mut reader = tokio::fs::read_dir(path).await?;
    let mut paths = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        *count += 1;
        if *count > MAX_ENTRIES {
            return Err(io::Error::other("fixture exceeds 10000 entries"));
        }
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

async fn import_directory(
    catalog: &Admin,
    config: &Config,
    clock: &dyn Clock,
    root: &Path,
) -> io::Result<SeedOutcome> {
    if !tokio::fs::symlink_metadata(root)
        .await?
        .file_type()
        .is_dir()
    {
        return Err(io::Error::other(
            "seed_dir must be a directory, not a symlink",
        ));
    }
    let root = tokio::fs::canonicalize(root).await?;
    let data = tokio::fs::canonicalize(&config.data_dir).await?;
    if crate::source_tree::paths_overlap_canonical(&root, &data) {
        return Err(io::Error::other("seed_dir and data_dir must not overlap"));
    }
    let mut count = 0;
    let mut pending: Vec<_> = entries(&root, &mut count)
        .await?
        .into_iter()
        .rev()
        .map(|path| (path, None::<String>, String::new(), 0))
        .collect();
    let (mut buckets, mut objects, mut existing_objects) = (0, 0, 0);
    while let Some((path, bucket, prefix, depth)) = pending.pop() {
        let label = path.strip_prefix(&root).expect("fixture path").display();
        let result = async {
            let leaf = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other("fixture names must be UTF-8"))?;
            let kind = tokio::fs::symlink_metadata(&path).await?.file_type();
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err(io::Error::other(
                    "only regular files and directories are supported",
                ));
            }
            let (bucket, name) = if let Some(bucket) = bucket {
                let name = if prefix.is_empty() {
                    leaf.to_owned()
                } else {
                    format!("{prefix}/{leaf}")
                };
                validate_object_name(&name).map_err(|error| io::Error::other(error.message))?;
                (bucket, name)
            } else {
                if !kind.is_dir() {
                    return Err(io::Error::other(
                        "top-level entries must be bucket directories",
                    ));
                }
                let name = BucketName::new(leaf).map_err(io::Error::other)?;
                let record = BucketDefinition::default_for(&name, &config.seed_project, clock)
                    .map_err(io::Error::other)?;
                match catalog.ensure_bucket(record).await {
                    Ok(EnsureBucket::Created(_)) => buckets += 1,
                    Ok(EnsureBucket::Existing(existing)) => {
                        if existing.project != config.seed_project {
                            return Err(io::Error::other(
                                "existing seed bucket belongs to another project",
                            ));
                        }
                    }
                    Err(error) => return Err(io::Error::other(error)),
                }
                (leaf.to_owned(), String::new())
            };
            if kind.is_dir() {
                if depth > MAX_DEPTH {
                    return Err(io::Error::other(
                        "fixture exceeds 32 directory levels below a bucket",
                    ));
                }
                for child in entries(&path, &mut count).await?.into_iter().rev() {
                    pending.push((child, Some(bucket.clone()), name.clone(), depth + 1));
                }
            } else {
                let bucket = BucketName::new(bucket).map_err(io::Error::other)?;
                let name = ObjectName::new(name).map_err(io::Error::other)?;
                match catalog
                    .check_upload(&bucket, &name, Some(GenerationCondition::Absent))
                    .await
                {
                    Ok(()) => {}
                    Err(AdminError::Precondition) => {
                        existing_objects += 1;
                        return Ok(());
                    }
                    Err(error) => return Err(io::Error::other(error)),
                }
                let mut file = tokio::fs::File::open(&path).await?;
                let metadata = file.metadata().await?;
                if !metadata.is_file() || metadata.len() > config.max_object_bytes {
                    return Err(io::Error::other(
                        "seed file is not regular or exceeds max_object_bytes",
                    ));
                }
                let (send, body) = mpsc::channel(2);
                let produce = async move {
                    let mut buffer = vec![0; 64 * 1024];
                    loop {
                        let read = file.read(&mut buffer).await?;
                        let chunk = if read == 0 {
                            UploadChunk::End
                        } else {
                            UploadChunk::Data(Bytes::copy_from_slice(&buffer[..read]))
                        };
                        if send.send(Ok(chunk)).await.is_err() || read == 0 {
                            return Ok::<_, io::Error>(());
                        }
                    }
                };
                let publish = catalog.publish_object(
                    &bucket,
                    &name,
                    Publication {
                        content_type: "application/octet-stream".into(),
                        expected: ExpectedHashes::default(),
                        condition: Some(GenerationCondition::Absent),
                        metadata: Default::default(),
                        max_bytes: config.max_object_bytes,
                        mode: PublishMode::Replace,
                    },
                    body,
                );
                let (produced, published) = tokio::join!(produce, publish);
                produced?;
                published.map_err(io::Error::other)?;
                objects += 1;
            }
            Ok::<_, io::Error>(())
        }
        .await;
        result.map_err(|error| io::Error::other(format!("{label}: {error}")))?;
    }
    catalog.finish_seed().await.map_err(io::Error::other)?;
    Ok(SeedOutcome::Imported {
        buckets,
        objects,
        existing_objects,
    })
}
