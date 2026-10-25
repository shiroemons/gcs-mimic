//! Local administrative fixture import, completed before the listener binds.

use crate::{
    clock::Clock,
    config::Config,
    json::validate_object_name,
    object::ExpectedHashes,
    source_tree::{WalkError, Walker, is_os_metadata},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BucketDefinition, BucketName, EnsureBucket, GenerationCondition,
            ObjectName, Origin, Publication, PublishMode,
        },
    },
};
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;

const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;
type Entry = io::Result<(PathBuf, std::fs::Metadata, usize)>;

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum SeedOutcome {
    Disabled,
    Skipped,
    Imported {
        buckets: usize,
        objects: usize,
        existing_objects: usize,
        skipped_os_metadata: usize,
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

fn walk_error(error: WalkError) -> io::Error {
    match error {
        WalkError::Io(error) => error,
        WalkError::EntryLimit => io::Error::other("fixture exceeds 10000 entries"),
        WalkError::DepthLimit => {
            io::Error::other("fixture exceeds 32 directory levels below a bucket")
        }
        WalkError::NotDirectory => io::Error::other("seed_dir must be a directory, not a symlink"),
    }
}

fn entries(root: PathBuf) -> io::Result<mpsc::Receiver<Entry>> {
    let (send, receive) = mpsc::channel(2);
    std::thread::Builder::new()
        .name("seed-walk".into())
        .spawn(move || {
            let walk = match Walker::new(&root, MAX_ENTRIES, MAX_DEPTH) {
                Ok(walk) => walk,
                Err(error) => {
                    let _ = send.blocking_send(Err(walk_error(error)));
                    return;
                }
            };
            for entry in walk {
                if send.blocking_send(entry.map_err(walk_error)).is_err() {
                    break;
                }
            }
        })?;
    Ok(receive)
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
    let mut entries = entries(root.clone())?;
    let (mut buckets, mut objects, mut existing_objects, mut skipped_os_metadata) = (0, 0, 0, 0);
    while let Some(entry) = entries.recv().await {
        let (path, metadata, depth) = entry?;
        let label = path.strip_prefix(&root).expect("fixture path").display();
        let result = async {
            let leaf = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other("fixture names must be UTF-8"))?;
            let kind = metadata.file_type();
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err(io::Error::other(
                    "only regular files and directories are supported",
                ));
            }
            if kind.is_file() && is_os_metadata(leaf) {
                skipped_os_metadata += 1;
                return Ok(());
            }
            let relative = path.strip_prefix(&root).map_err(io::Error::other)?;
            let mut components = relative.iter();
            let bucket = components
                .next()
                .and_then(|part| part.to_str())
                .ok_or_else(|| io::Error::other("fixture names must be UTF-8"))?;
            let (bucket, name) = if depth > 0 {
                let name = components
                    .map(|part| {
                        part.to_str()
                            .ok_or_else(|| io::Error::other("fixture names must be UTF-8"))
                    })
                    .collect::<io::Result<Vec<_>>>()?
                    .join("/");
                validate_object_name(&name).map_err(|error| io::Error::other(error.message))?;
                (bucket.to_owned(), name)
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
            if kind.is_file() {
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
                let file = tokio::fs::File::open(&path).await?;
                let metadata = file.metadata().await?;
                if !metadata.is_file() || metadata.len() > config.max_object_bytes {
                    return Err(io::Error::other(
                        "seed file is not regular or exceeds max_object_bytes",
                    ));
                }
                catalog
                    .publish_from_file(
                        &bucket,
                        &name,
                        Publication {
                            content_type: config.seed_content_type.for_name(leaf).into(),
                            expected: ExpectedHashes::default(),
                            condition: Some(GenerationCondition::Absent),
                            metadata: Default::default(),
                            max_bytes: config.max_object_bytes,
                            mode: PublishMode::Replace,
                        },
                        file.into_std().await,
                    )
                    .await
                    .map_err(io::Error::other)?;
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
        skipped_os_metadata,
    })
}
