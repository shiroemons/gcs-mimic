//! Root identity, exclusive ownership and fail-closed baseline activation.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{CatalogId, SyncState, catalog_id};
use crate::{
    bucket::CatalogError,
    fsutil::{self, FileIdentity, FsError},
    storage::{
        admin::{AdminError, WAIT_TIMEOUT},
        lock,
    },
    sync::mapping::MARKER,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_roots (
    root_id BLOB PRIMARY KEY CHECK(typeof(root_id) = 'blob' AND length(root_id) = 16),
    location BLOB NOT NULL UNIQUE CHECK(typeof(location) = 'blob'),
    device BLOB NOT NULL CHECK(typeof(device) = 'blob' AND length(device) = 8),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    complete INTEGER NOT NULL CHECK(complete IN (0, 1)),
    UNIQUE(device, inode)
) WITHOUT ROWID;
PRAGMA user_version = 19;";

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection
        .transaction()
        .map_err(super::super::unavailable)?;
    transaction
        .execute_batch(SCHEMA)
        .map_err(super::super::unavailable)?;
    transaction.commit().map_err(super::super::unavailable)
}

#[cfg(test)]
static FAILURE: std::sync::Mutex<Option<(PathBuf, &'static str)>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn failure_boundary(path: &Path, phase: &str) -> Result<(), RootError> {
    let failure = FAILURE.lock().map_err(|_| RootError::InvalidMarker)?;
    if failure
        .as_ref()
        .is_some_and(|(selected, step)| selected == path && *step == phase)
    {
        return Err(io::Error::other("injected activation interruption").into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootId([u8; 16]);
impl RootId {
    pub fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}
impl std::fmt::Display for RootId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::hashing::encode_lower(&self.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    Activating,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootStatus {
    pub catalog_id: CatalogId,
    pub root_id: RootId,
    pub baseline: Baseline,
}
impl RootStatus {
    pub fn permits_external_deletions(self) -> bool {
        self.baseline == Baseline::Complete
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    Open,
    Reinitialize { catalog_id: CatalogId },
}

#[derive(Debug, PartialEq, Eq)]
pub enum RootError {
    Admin(AdminError),
    InUse,
    Unsupported,
    RootChanged,
    MissingMarker,
    InvalidMarker,
    ForeignMarker,
    ReinitializeDenied,
}
impl From<AdminError> for RootError {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}
impl From<CatalogError> for RootError {
    fn from(error: CatalogError) -> Self {
        Self::Admin(error.into())
    }
}
impl From<io::Error> for RootError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock => Self::InUse,
            io::ErrorKind::Unsupported => Self::Unsupported,
            _ => Self::Admin(error.into()),
        }
    }
}
impl From<FsError> for RootError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::Io(error) => error.into(),
            FsError::Unsupported => Self::Unsupported,
        }
    }
}
impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "synchronization root: {self:?}")
    }
}
impl std::error::Error for RootError {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    catalog_id: String,
    root_id: String,
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_marker(directory: &File) -> Result<Option<(Marker, FileIdentity)>, RootError> {
    let file = match fsutil::open_regular_at(directory, MARKER.as_ref()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RootError::InvalidMarker),
    };
    let before = fsutil::stat_identity(&file)?;
    let mut bytes = Vec::with_capacity(4097);
    (&file)
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(RootError::from)?;
    if bytes.len() > 4096 || before != fsutil::stat_identity(&file)? {
        return Err(RootError::InvalidMarker);
    }
    let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| RootError::InvalidMarker)?;
    if marker.version != 1 || !valid_id(&marker.catalog_id) || !valid_id(&marker.root_id) {
        return Err(RootError::InvalidMarker);
    }
    let current = fsutil::open_regular_at(directory, MARKER.as_ref())
        .map_err(|_| RootError::InvalidMarker)?;
    if fsutil::stat_identity(&current)? != before {
        return Err(RootError::InvalidMarker);
    }
    Ok(Some((marker, before)))
}

struct Ownership {
    lock: lock::DirectoryLock,
    path: PathBuf,
    selected_path: PathBuf,
    identity: FileIdentity,
    _catalog_permit: OwnedSemaphorePermit,
}

fn reserve(
    connection: &mut Connection,
    location: Vec<u8>,
    identity: FileIdentity,
    marker: Option<(String, String)>,
    catalog: CatalogId,
    activation: Activation,
) -> Result<RootStatus, RootError> {
    let transaction = connection
        .transaction()
        .map_err(super::super::unavailable)?;
    if catalog_id(&transaction)? != catalog {
        return Err(RootError::ForeignMarker);
    }
    let found = transaction
        .query_row(
            "SELECT root_id, device, inode, complete, location FROM sync_roots
         WHERE location = ?1 OR (device = ?2 AND inode = ?3) LIMIT 1",
            params![
                location,
                identity.device.to_be_bytes(),
                identity.inode.to_be_bytes()
            ],
            |row| {
                Ok((
                    RootId(row.get(0)?),
                    row.get::<_, [u8; 8]>(1)?,
                    row.get::<_, [u8; 8]>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(super::super::unavailable)?;
    let status = if let Some((id, device, inode, complete, stored_location)) = found {
        if device != identity.device.to_be_bytes()
            || inode != identity.inode.to_be_bytes()
            || stored_location != location
        {
            return Err(RootError::RootChanged);
        }
        let baseline = match (activation, marker) {
            (Activation::Open, Some((c, r))) if c == catalog.to_string() && r == id.to_string() => {
                if complete {
                    Baseline::Complete
                } else {
                    Baseline::Activating
                }
            }
            (Activation::Open, None) => return Err(RootError::MissingMarker),
            (Activation::Reinitialize { catalog_id }, None) if catalog_id == catalog => {
                transaction
                    .execute(
                        "UPDATE sync_roots SET complete = 0 WHERE root_id = ?1",
                        [id.0],
                    )
                    .map_err(super::super::unavailable)?;
                Baseline::Activating
            }
            (Activation::Reinitialize { .. }, _) => return Err(RootError::ReinitializeDenied),
            _ => return Err(RootError::ForeignMarker),
        };
        RootStatus {
            catalog_id: catalog,
            root_id: id,
            baseline,
        }
    } else {
        if marker.is_some() {
            return Err(RootError::ForeignMarker);
        }
        if activation != Activation::Open {
            return Err(RootError::ReinitializeDenied);
        }
        let mut id = [0; 16];
        getrandom::fill(&mut id)
            .map_err(|error| RootError::Admin(AdminError::Backend(error.to_string())))?;
        transaction
            .execute(
                "INSERT INTO sync_roots VALUES (?1,?2,?3,?4,0)",
                params![
                    id,
                    location,
                    identity.device.to_be_bytes(),
                    identity.inode.to_be_bytes()
                ],
            )
            .map_err(super::super::unavailable)?;
        RootStatus {
            catalog_id: catalog,
            root_id: RootId(id),
            baseline: Baseline::Activating,
        }
    };
    transaction.commit().map_err(super::super::unavailable)?;
    Ok(status)
}
impl Ownership {
    fn validate(&self) -> Result<(), RootError> {
        for path in [&self.path, &self.selected_path] {
            let current =
                fsutil::open_directory_nofollow(path).map_err(|_| RootError::RootChanged)?;
            let identity = fsutil::stat_identity(&current)?;
            if identity.device != self.identity.device || identity.inode != self.identity.inode {
                return Err(RootError::RootChanged);
            }
        }
        Ok(())
    }
}

/// Clones retain both root ownership and catalog shutdown ownership.
#[derive(Clone)]
pub struct RootLease {
    ownership: Arc<Ownership>,
    state: SyncState,
    status: RootStatus,
    marker_identity: FileIdentity,
}
impl RootLease {
    pub(super) fn catalog(&self) -> &super::super::Catalog {
        &self.state.0
    }

    /// A conservative snapshot; only explicit completion returns Complete.
    pub fn status(&self) -> RootStatus {
        self.status
    }

    fn validate(&self) -> Result<(), RootError> {
        self.ownership.validate()?;
        let (marker, identity) =
            read_marker(self.ownership.lock.file())?.ok_or(RootError::MissingMarker)?;
        if marker.catalog_id != self.status.catalog_id.to_string()
            || marker.root_id != self.status.root_id.to_string()
        {
            return Err(RootError::ForeignMarker);
        }
        if identity != self.marker_identity {
            return Err(RootError::InvalidMarker);
        }
        Ok(())
    }

    /// Runs descriptor-relative work on the bounded dedicated workers, owning the lease until done.
    /// Callers must not retain descriptors or start work outside this owned closure.
    pub async fn file_job<T: Send + 'static>(
        &self,
        job: impl FnOnce(&File) -> Result<T, AdminError> + Send + 'static,
    ) -> Result<T, RootError> {
        let lease = self.clone();
        self.state
            .0
            .0
            .file_workers
            .run(move || {
                Ok((|| {
                    lease.validate()?;
                    let result = job(lease.ownership.lock.file()).map_err(RootError::from)?;
                    lease.validate()?;
                    Ok(result)
                })())
            })
            .await?
    }

    /// The adapter calls this only after its complete durable initial merge.
    pub async fn complete_baseline(&self) -> Result<RootStatus, RootError> {
        self.file_job(|_| Ok(())).await?;
        let id = self.status.root_id;
        let lease = self.clone();
        self.state
            .0
            .run_admin(move |connection| {
                let _lease = lease;
                let changed = connection
                    .execute(
                        "UPDATE sync_roots SET complete = 1 WHERE root_id = ?1",
                        [id.0],
                    )
                    .map_err(super::super::unavailable)?;
                if changed != 1 {
                    return Err(CatalogError::Corrupt("missing root association".into()));
                }
                Ok(())
            })
            .await?;
        Ok(RootStatus {
            baseline: Baseline::Complete,
            ..self.status
        })
    }
}

impl SyncState {
    pub async fn activate_root(
        &self,
        path: impl AsRef<Path>,
        activation: Activation,
    ) -> Result<RootLease, RootError> {
        let permit = self
            .0
            .wait_admin(
                self.0.0.admin_reads.clone(),
                tokio::time::Instant::now() + WAIT_TIMEOUT,
            )
            .await?;
        let path = path.as_ref().to_owned();
        let data_dir = self.0.0.data_dir.clone();
        let catalog = self.catalog_id().await?;
        // Detached ownership makes cancellation safe across the separate durability domains.
        let state = self.clone();
        tokio::spawn(async move {
            let ownership = state
                .0
                .0
                .file_workers
                .run(move || {
                    Ok((|| {
                        let selected_path = std::path::absolute(&path)?;
                        let lock = lock::acquire_directory(&selected_path)?;
                        let canonical = selected_path.canonicalize()?;
                        if crate::source_tree::paths_overlap(&canonical, &data_dir)? {
                            return Err(RootError::RootChanged);
                        }
                        let identity = fsutil::stat_identity(lock.file())?;
                        let ownership = Ownership {
                            lock,
                            path: canonical,
                            selected_path,
                            identity,
                            _catalog_permit: permit,
                        };
                        ownership.validate()?;
                        let marker = read_marker(ownership.lock.file())?;
                        Ok((Arc::new(ownership), marker))
                    })())
                })
                .await??;
            state
                .activate_owned(ownership.0, ownership.1, catalog, activation)
                .await
        })
        .await
        .map_err(|error| RootError::Admin(AdminError::Backend(error.to_string())))?
    }

    async fn activate_owned(
        &self,
        ownership: Arc<Ownership>,
        marker: Option<(Marker, FileIdentity)>,
        catalog: CatalogId,
        activation: Activation,
    ) -> Result<RootLease, RootError> {
        let location = ownership.path.as_os_str().as_encoded_bytes().to_vec();
        let identity = ownership.identity;
        let marker_ids = marker
            .as_ref()
            .map(|(m, _)| (m.catalog_id.clone(), m.root_id.clone()));
        let status = self
            .0
            .run_admin(move |connection| {
                Ok(reserve(
                    connection, location, identity, marker_ids, catalog, activation,
                ))
            })
            .await??;
        let owns = ownership.clone();
        let marker_identity = self
            .0
            .0
            .file_workers
            .run(move || {
                Ok((|| {
                    owns.validate()?;
                    #[cfg(test)]
                    failure_boundary(&owns.path, "reserved")?;
                    if marker.is_none() {
                        let bytes = serde_json::to_vec(&Marker {
                            version: 1,
                            catalog_id: catalog.to_string(),
                            root_id: status.root_id.to_string(),
                        })
                        .map_err(|_| RootError::InvalidMarker)?;
                        let mut file =
                            fsutil::create_regular_at(owns.lock.file(), MARKER.as_ref())?;
                        #[cfg(test)]
                        failure_boundary(&owns.path, "created")?;
                        file.write_all(&bytes)?;
                        file.sync_all()?;
                        #[cfg(test)]
                        failure_boundary(&owns.path, "file_synced")?;
                        owns.lock.file().sync_all()?;
                        #[cfg(test)]
                        failure_boundary(&owns.path, "directory_synced")?;
                    }
                    let (current, identity) =
                        read_marker(owns.lock.file())?.ok_or(RootError::MissingMarker)?;
                    if current.catalog_id != catalog.to_string()
                        || current.root_id != status.root_id.to_string()
                    {
                        return Err(RootError::ForeignMarker);
                    }
                    if marker
                        .as_ref()
                        .is_some_and(|(_, previous)| *previous != identity)
                    {
                        return Err(RootError::InvalidMarker);
                    }
                    owns.validate()?;
                    Ok(identity)
                })())
            })
            .await??;
        Ok(RootLease {
            ownership,
            state: self.clone(),
            status,
            marker_identity,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "sync_root_tests.rs"]
mod tests;
