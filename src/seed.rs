//! Local administrative fixture import, completed before the listener binds.

use crate::{
    clock::Clock,
    config::Config,
    hashing::{Hasher, Hashes},
    json::validate_object_name,
    object::{ExpectedHashes, ObjectRecord},
    source_tree::{WalkError, Walker, is_os_metadata},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BatchPublication, BucketDefinition, BucketName, EnsureBucket,
            Generation, GenerationCondition, ObjectName, Origin, Publication, PublishMode,
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
    Imported {
        buckets: usize,
        objects: usize,
        replaced: usize,
        unchanged: usize,
        blocked: usize,
        skipped_os_metadata: usize,
    },
}

#[derive(Default)]
struct Counts {
    buckets: usize,
    objects: usize,
    replaced: usize,
    unchanged: usize,
    blocked: usize,
    skipped_os_metadata: usize,
}

/// Replays the seed tree on every startup, like fake-gcs-server's `-data`: fixture files win
/// over API changes at the same name, while unrelated objects are never deleted.
pub async fn import(
    catalog: &Catalog,
    config: &Config,
    clock: &dyn Clock,
) -> io::Result<SeedOutcome> {
    let admin = catalog.admin(Origin::Seed);
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

/// True when the live object carries exactly the metadata a seed publication would write, so
/// API metadata edits at a fixture name are reverted like fake-gcs-server's overwrite. Holds and
/// storage class come from bucket defaults and are deliberately not compared.
fn matches_seed(live: &ObjectRecord, content_type: &str) -> bool {
    live.content_type == content_type
        && live.content_encoding.is_none()
        && live.content_disposition.is_none()
        && live.content_language.is_none()
        && live.cache_control.is_none()
        && live.custom_time.is_none()
        && live.metadata.is_empty()
}

/// Hashes with a fixed buffer and rewinds, so the same descriptor can be staged afterwards.
fn hash_file(file: &mut std::fs::File) -> io::Result<Hashes> {
    use std::io::{Read, Seek};
    let mut hasher = Hasher::default();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.rewind()?;
    Ok(hasher.finish())
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
    let mut counts = Counts::default();
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
                counts.skipped_os_metadata += 1;
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
                // fake-gcs-server ignores top-level files, and an implicit /data mount often holds
                // a README or .gitkeep, so skipping keeps such trees startable.
                if !kind.is_dir() {
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "seed_skipped",
                        serde_json::json!({
                            "path": label.to_string(),
                            "reason": "top-level entries must be bucket directories",
                        }),
                    );
                    return Ok(());
                }
                let name = BucketName::new(leaf).map_err(io::Error::other)?;
                let record = BucketDefinition::default_for(&name, &config.seed_project, clock)
                    .map_err(io::Error::other)?;
                match catalog.ensure_bucket(record).await {
                    Ok(EnsureBucket::Created(_)) => counts.buckets += 1,
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
                let live = match catalog.get_object(&bucket, &name, None).await {
                    Ok(record) => Some(record),
                    Err(AdminError::NotFound) => None,
                    Err(error) => return Err(io::Error::other(error)),
                };
                let file = tokio::fs::File::open(&path).await?;
                let metadata = file.metadata().await?;
                if !metadata.is_file() || metadata.len() > config.max_object_bytes {
                    return Err(io::Error::other(
                        "seed file is not regular or exceeds max_object_bytes",
                    ));
                }
                let content_type = config.seed_content_type.for_name(leaf);
                // Compare before staging or publishing: a replay of an unchanged tree must not copy
                // bodies or mint generations, and the retention check would otherwise report a
                // held but unchanged object as blocked.
                let mut file = file.into_std().await;
                if let Some(live) = &live
                    && live.size == metadata.len()
                    && matches_seed(live, content_type)
                {
                    let (hashes, rewound) = tokio::task::spawn_blocking(move || {
                        hash_file(&mut file).map(|hashes| (hashes, file))
                    })
                    .await
                    .map_err(io::Error::other)??;
                    if live.md5 == hashes.md5 && live.crc32c == hashes.crc32c {
                        counts.unchanged += 1;
                        return Ok(());
                    }
                    file = rewound;
                }
                let staged = catalog
                    .stage_from_file(file, ExpectedHashes::default(), config.max_object_bytes)
                    .await
                    .map_err(io::Error::other)?;
                let condition = match &live {
                    Some(live) => GenerationCondition::Is(
                        Generation::new(live.generation).map_err(io::Error::other)?,
                    ),
                    None => GenerationCondition::Absent,
                };
                let mut published = catalog
                    .publish_objects(vec![BatchPublication {
                        bucket,
                        name,
                        request: Publication {
                            content_type: content_type.into(),
                            expected: ExpectedHashes::default(),
                            condition: Some(condition),
                            metadata: Default::default(),
                            max_bytes: config.max_object_bytes,
                            mode: PublishMode::Replace,
                        },
                        staged,
                    }])
                    .await
                    .map_err(io::Error::other)?;
                let result = published
                    .results
                    .pop()
                    .ok_or_else(|| io::Error::other("single seed publication was not attempted"))?;
                match result {
                    Ok(_) if live.is_some() => counts.replaced += 1,
                    Ok(_) => counts.objects += 1,
                    Err(AdminError::Hold) => {
                        counts.blocked += 1;
                        crate::diagnostics::emit(
                            crate::diagnostics::LogLevel::Warn,
                            "seed_blocked",
                            serde_json::json!({ "path": label.to_string() }),
                        );
                    }
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
            Ok::<_, io::Error>(())
        }
        .await;
        result.map_err(|error| io::Error::other(format!("{label}: {error}")))?;
    }
    catalog.finish_seed().await.map_err(io::Error::other)?;
    Ok(SeedOutcome::Imported {
        buckets: counts.buckets,
        objects: counts.objects,
        replaced: counts.replaced,
        unchanged: counts.unchanged,
        blocked: counts.blocked,
        skipped_os_metadata: counts.skipped_os_metadata,
    })
}
