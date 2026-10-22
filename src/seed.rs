//! Local administrative fixture import, completed before the listener binds.

use crate::{
    bucket::CatalogError,
    clock::Clock,
    config::Config,
    json::{parse_insert, validate_object_name},
    object::{ExpectedHashes, ObjectConditions, UploadChunk},
    storage::Catalog,
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::{io::AsyncReadExt, sync::mpsc};

const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;

pub async fn import(catalog: &Catalog, config: &Config, clock: &dyn Clock) -> io::Result<Value> {
    if catalog.seed_complete().await.map_err(io::Error::other)? {
        return Ok(json!({"outcome":"skipped"}));
    }
    let Some(root) = &config.seed_dir else {
        catalog.finish_seed().await.map_err(io::Error::other)?;
        return Ok(json!({"outcome":"disabled"}));
    };
    eprintln!("{{\"event\":\"seeding\"}}");
    let result = import_directory(catalog, config, clock, root).await;
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
    catalog: &Catalog,
    config: &Config,
    clock: &dyn Clock,
    root: &Path,
) -> io::Result<Value> {
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
    if root.starts_with(&data) || data.starts_with(&root) {
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
                let record = parse_insert(
                    json!({"name":leaf}).to_string().as_bytes(),
                    &config.seed_project,
                    clock.now(),
                )
                .map_err(|error| io::Error::other(error.message))?;
                match catalog.insert_bucket(record).await {
                    Ok(_) => buckets += 1,
                    Err(CatalogError::AlreadyExists) => {
                        let existing = catalog
                            .get_bucket(leaf)
                            .await
                            .map_err(io::Error::other)?
                            .ok_or_else(|| io::Error::other("seed bucket disappeared"))?;
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
                let conditions = ObjectConditions {
                    if_generation_match: Some(0),
                    ..ObjectConditions::default()
                };
                match catalog
                    .check_object_upload(&bucket, &name, conditions)
                    .await
                {
                    Ok(()) => {}
                    Err(crate::object::ObjectError::PreconditionFailed) => {
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
                let publish = catalog.put_object(
                    &bucket,
                    &name,
                    "application/octet-stream".into(),
                    ExpectedHashes::default(),
                    conditions,
                    config.max_object_bytes,
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
    Ok(
        json!({"outcome":"imported", "buckets":buckets, "objects":objects, "existing_objects":existing_objects}),
    )
}
