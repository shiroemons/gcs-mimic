//! Root identity, exclusive ownership and fail-closed baseline activation.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{CatalogId, SyncState, catalog_id};
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    fsutil::{self, FileIdentity, FsError},
    storage::{admin::AdminError, lock},
    sync::mapping::{MARKER, MARKER_STAGING},
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

crate::storage::opaque_id::opaque_id! {
    /// The identity of one synchronized directory tree, stored as 16 raw bytes.
    pub struct RootId([u8; 16]);
    invalid = "invalid synchronization root ID";
    column = blob;
}
impl RootId {
    fn generate() -> Result<Self, getrandom::Error> {
        crate::storage::opaque_id::random().map(Self)
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
    #[cfg(all(test, unix))]
    pub(crate) fn permits_external_deletions(self) -> bool {
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
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "io::ErrorKind is non_exhaustive; every kind but Unsupported is a storage failure"
    )]
    fn from(error: io::Error) -> Self {
        match error.kind() {
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
        f.write_str("synchronization root: ")?;
        match self {
            Self::Admin(_) => f.write_str("admin operation failed"),
            Self::InUse => f.write_str("InUse"),
            Self::Unsupported => f.write_str("Unsupported"),
            Self::RootChanged => f.write_str("RootChanged"),
            Self::MissingMarker => f.write_str("MissingMarker"),
            Self::InvalidMarker => f.write_str("InvalidMarker"),
            Self::ForeignMarker => f.write_str("ForeignMarker"),
            Self::ReinitializeDenied => f.write_str("ReinitializeDenied"),
        }
    }
}
impl std::error::Error for RootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Admin(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    catalog_id: String,
    root_id: String,
}

fn valid_id(id: &str) -> bool {
    crate::hex::is_lower(id, 32)
}

fn read_marker(directory: &File) -> Result<Option<(Marker, FileIdentity)>, RootError> {
    read_marker_name(directory, MARKER)
}
fn marker_open_error(error: io::Error) -> RootError {
    if error.kind() == io::ErrorKind::NotFound || crate::sync::tree::structural_entry_error(&error)
    {
        RootError::InvalidMarker
    } else {
        error.into()
    }
}
fn read_marker_name(
    directory: &File,
    name: &str,
) -> Result<Option<(Marker, FileIdentity)>, RootError> {
    let file = match fsutil::open_regular_at(directory, name.as_ref()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(marker_open_error(error)),
    };
    let before = fsutil::stat_identity(&file)?;
    let mut bytes = Vec::with_capacity(4097);
    (&file).take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 || before != fsutil::stat_identity(&file)? {
        return Err(RootError::InvalidMarker);
    }
    let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| RootError::InvalidMarker)?;
    if marker.version != 1 || !valid_id(&marker.catalog_id) || !valid_id(&marker.root_id) {
        return Err(RootError::InvalidMarker);
    }
    let current = fsutil::open_regular_at(directory, name.as_ref()).map_err(marker_open_error)?;
    if fsutil::stat_identity(&current)? != before {
        return Err(RootError::InvalidMarker);
    }
    Ok(Some((marker, before)))
}

#[derive(Clone)]
enum MarkerState {
    Published(Marker, FileIdentity),
    Staged(Marker),
    Absent,
}
fn marker_state(directory: &File) -> Result<MarkerState, RootError> {
    if let Some((marker, identity)) = read_marker(directory)? {
        return Ok(MarkerState::Published(marker, identity));
    }
    match read_marker_name(directory, MARKER_STAGING) {
        Ok(Some((marker, _))) => Ok(MarkerState::Staged(marker)),
        Ok(None) => Ok(MarkerState::Absent),
        Err(RootError::InvalidMarker) => {
            fsutil::unlink_file_at(directory, MARKER_STAGING.as_ref())?;
            Ok(MarkerState::Absent)
        }
        Err(error) => Err(error),
    }
}
fn remove_staging(directory: &File) -> Result<(), RootError> {
    match fsutil::unlink_file_at(directory, MARKER_STAGING.as_ref()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn publish_marker(
    owns: &Ownership,
    status: RootStatus,
    marker: &MarkerState,
) -> Result<FileIdentity, RootError> {
    owns.validate()?;
    let root = owns.lock.file();
    #[cfg(test)]
    failure_boundary(&owns.path, "reserved")?;
    if matches!(marker, MarkerState::Absent) {
        remove_staging(root)?;
        let bytes = serde_json::to_vec(&Marker {
            version: 1,
            catalog_id: status.catalog_id.to_string(),
            root_id: status.root_id.to_string(),
        })
        .map_err(AdminError::backend)?;
        let mut file = fsutil::create_regular_at(root, MARKER_STAGING.as_ref())?;
        #[cfg(test)]
        {
            #[cfg(unix)]
            super::super::fault::file_created(&owns.path.join(MARKER_STAGING));
            failure_boundary(&owns.path, "created")?;
            super::super::fault::hit_io(&owns.path, super::super::fault::Step::SyncMarkerWrite)?;
        }
        file.write_all(&bytes)?;
        #[cfg(test)]
        super::super::fault::hit_io(&owns.path, super::super::fault::Step::SyncMarkerSync)?;
        file.sync_all()?;
        root.sync_all()?;
        #[cfg(test)]
        {
            super::super::fault::file_synced(&owns.path, &file.metadata()?);
            #[cfg(unix)]
            super::super::fault::dir_synced(&owns.path);
            failure_boundary(&owns.path, "file_synced")?;
            super::super::fault::hit_io(&owns.path, super::super::fault::Step::SyncMarkerPrepared)?;
        }
    }
    if !matches!(marker, MarkerState::Published(..)) {
        #[cfg(test)]
        super::super::fault::hit_io(&owns.path, super::super::fault::Step::SyncMarkerRename)?;
        fsutil::durable_replace_between(root, MARKER_STAGING.as_ref(), root, MARKER.as_ref(), None)
            .map_err(|error| match error {
                FsError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    RootError::ForeignMarker
                }
                error => error.into(),
            })?;
        #[cfg(test)]
        {
            #[cfg(unix)]
            super::super::fault::renamed(&owns.path.join(MARKER_STAGING), &owns.path.join(MARKER));
            failure_boundary(&owns.path, "renamed")?;
            super::super::fault::hit_io(
                &owns.path,
                super::super::fault::Step::SyncMarkerParentSync,
            )?;
        }
        root.sync_all()?;
        #[cfg(test)]
        {
            #[cfg(unix)]
            super::super::fault::dir_synced(&owns.path);
            failure_boundary(&owns.path, "directory_synced")?;
            super::super::fault::hit_io(
                &owns.path,
                super::super::fault::Step::SyncMarkerPublished,
            )?;
        }
    }
    let (current, identity) = read_marker(root)?.ok_or(RootError::MissingMarker)?;
    if current.catalog_id != status.catalog_id.to_string()
        || current.root_id != status.root_id.to_string()
    {
        return Err(RootError::ForeignMarker);
    }
    if matches!(marker,MarkerState::Published(_,previous) if *previous!=identity) {
        return Err(RootError::InvalidMarker);
    }
    owns.validate()?;
    Ok(identity)
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
    location: &[u8],
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
    let found = sql::row(
        &transaction,
        "SELECT root_id, device, inode, complete, location FROM sync_roots
         WHERE location = ?1 OR (device = ?2 AND inode = ?3) LIMIT 1",
        params![
            location,
            identity.device.to_be_bytes(),
            identity.inode.to_be_bytes()
        ],
        |row| {
            Ok((
                row.get::<_, RootId>(0)?,
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
                sql::exec(
                    &transaction,
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
        let id =
            RootId::generate().map_err(|error| RootError::Admin(AdminError::backend(error)))?;
        sql::exec(
            &transaction,
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
            root_id: id,
            baseline: Baseline::Activating,
        }
    };
    transaction.commit().map_err(super::super::unavailable)?;
    Ok(status)
}
impl Ownership {
    fn validate(&self) -> Result<(), RootError> {
        for path in [&self.path, &self.selected_path] {
            let current = fsutil::open_directory_nofollow(path).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound
                    || crate::sync::tree::structural_entry_error(&error)
                {
                    RootError::RootChanged
                } else {
                    error.into()
                }
            })?;
            let identity = fsutil::stat_identity(&current)?;
            if identity.device != self.identity.device || identity.inode != self.identity.inode {
                return Err(RootError::RootChanged);
            }
        }
        Ok(())
    }
}

impl RootLease {
    pub(crate) async fn revalidate(&self) -> Result<(), RootError> {
        self.file_job(|_| Ok(())).await
    }
    pub(crate) fn watch_path(&self) -> &Path {
        &self.ownership.path
    }
    pub(crate) fn observation_time_ns(&self) -> Result<i128, RootError> {
        let now = self.catalog().0.clock.now();
        match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => i128::try_from(duration.as_nanos())
                .map_err(|error| AdminError::backend(error).into()),
            Err(error) => i128::try_from(error.duration().as_nanos())
                .map(|nanos| -nanos)
                .map_err(|error| AdminError::backend(error).into()),
        }
    }
}

/// Clones retain both root ownership and catalog shutdown ownership.
#[derive(Clone)]
pub struct RootLease {
    ownership: Arc<Ownership>,
    state: SyncState,
    status: RootStatus,
    marker_identity: FileIdentity,
    authority: Arc<super::authority::ControlAuthority>,
    stopping: Option<tokio::sync::watch::Receiver<bool>>,
    tree_limits: crate::sync::tree::TreeLimits,
    pub(super) published_observation: super::observation::publication::Published,
}
impl RootLease {
    pub(crate) fn with_shutdown(mut self, stopping: tokio::sync::watch::Receiver<bool>) -> Self {
        self.stopping = Some(stopping);
        self
    }
    pub(crate) fn with_tree_limits(mut self, limits: crate::sync::tree::TreeLimits) -> Self {
        self.tree_limits = limits;
        self
    }
    pub(crate) fn tree_limits(&self) -> crate::sync::tree::TreeLimits {
        self.tree_limits
    }
    pub(crate) fn stop_signal(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.stopping.clone()
    }
    pub(crate) fn with_authority(&self, authority: super::authority::ControlAuthority) -> Self {
        Self {
            authority: Arc::new(authority),
            ..self.clone()
        }
    }
    pub(super) fn authority(&self) -> super::authority::ControlAuthority {
        self.authority.as_ref().clone()
    }
    pub(super) fn control_scope(&self) -> super::authority::ControlScope {
        super::authority::ControlScope::new(self.status.root_id, self.authority())
    }
    pub(crate) fn sync_admin(&self) -> crate::storage::admin::Admin {
        self.catalog()
            .admin(crate::storage::admin::Origin::SyncIngest)
            .with_sync_authority(self.control_scope())
    }
    pub(crate) async fn sync_binding(
        &self,
        bucket: &crate::storage::admin::BucketName,
    ) -> Result<super::SyncBinding, AdminError> {
        self.catalog()
            .sync_state()
            .binding(bucket)
            .await
            .map(|binding| binding.with_control(self.control_scope()))
    }
    #[cfg(test)]
    pub(super) fn fault_tree(&self) -> PathBuf {
        self.ownership.path.clone()
    }

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
    /// Directory descriptors stay in this closure. A staged regular source may escape only
    /// with its own RootLease clone, closing the source before releasing that clone.
    pub(crate) async fn file_job<T: Send + 'static>(
        &self,
        job: impl FnOnce(&File) -> Result<T, AdminError> + Send + 'static,
    ) -> Result<T, RootError> {
        if matches!(
            self.authority.as_ref(),
            super::authority::ControlAuthority::Strict { .. }
        ) {
            let scope = self.control_scope();
            self.catalog()
                .run_admin(move |connection| scope.authorize(connection))
                .await?;
        }
        let lease = self.clone();
        self.state
            .0
            .0
            .file_workers
            .run(move || {
                lease.validate()?;
                let result = job(lease.ownership.lock.file())?;
                lease.validate()?;
                Ok(result)
            })
            .await
    }

    /// The adapter calls this only after its complete durable initial merge.
    pub(crate) async fn complete_baseline(&self) -> Result<RootStatus, RootError> {
        self.revalidate().await?;
        let id = self.status.root_id;
        let lease = self.clone();
        self.state
            .0
            .run_admin(move |connection| {
                lease.control_scope().authorize(connection)?;
                let _lease = lease;
                let changed = sql::exec(
                    connection,
                    "UPDATE sync_roots SET complete = 1 WHERE root_id = ?1",
                    [id.0],
                )?;
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
            .wait_admin(self.0.0.admin_reads.clone(), self.0.admin_deadline())
            .await?;
        let path = path.as_ref().to_owned();
        let data_dir = self.0.0.data_dir.clone();
        let catalog = self.catalog_id().await?;
        // Detached ownership makes cancellation safe across the separate durability domains.
        let state = self.clone();
        crate::task::spawn_supervised("sync-root-ownership", async move {
            let ownership = state
                .0
                .0
                .file_workers
                .run(move || {
                    let selected_path = std::path::absolute(&path)?;
                    let lock = lock::acquire_directory(&selected_path).map_err(|error| {
                        if error.kind() == io::ErrorKind::WouldBlock {
                            RootError::InUse
                        } else {
                            error.into()
                        }
                    })?;
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
                    let marker = marker_state(ownership.lock.file())?;
                    Ok((Arc::new(ownership), marker))
                })
                .await?;
            state
                .activate_owned(ownership.0, ownership.1, catalog, activation)
                .await
        })
        .await
        .map_err(|error| RootError::Admin(AdminError::backend(error)))?
    }

    async fn activate_owned(
        &self,
        ownership: Arc<Ownership>,
        marker: MarkerState,
        catalog: CatalogId,
        activation: Activation,
    ) -> Result<RootLease, RootError> {
        let location = ownership.path.as_os_str().as_encoded_bytes().to_vec();
        let identity = ownership.identity;
        let marker_ids = match &marker {
            MarkerState::Published(m, _) | MarkerState::Staged(m) => {
                Some((m.catalog_id.clone(), m.root_id.clone()))
            }
            MarkerState::Absent => None,
        };
        let staged = matches!(marker, MarkerState::Staged(_));
        let (status,staging_valid) = self
            .0
            .run_admin_with_error(move |connection| {
                let valid=if staged {
                    let (catalog_id,root_id)=marker_ids.as_ref().ok_or(RootError::InvalidMarker)?;
                    catalog_id==&catalog.to_string() && sql::scalar::<bool>(connection,"SELECT EXISTS(SELECT 1 FROM sync_roots WHERE location=?1 AND device=?2 AND inode=?3 AND lower(hex(root_id))=?4)",params![location,identity.device.to_be_bytes(),identity.inode.to_be_bytes(),root_id]).map_err(super::super::unavailable)?
                } else {true};
                Ok::<_,RootError>((reserve(connection,&location,identity,if valid {marker_ids} else {None},catalog,activation),valid))
            })
            .await?;
        let marker = if staged && !staging_valid {
            let owns = ownership.clone();
            self.0
                .0
                .file_workers
                .run(move || remove_staging(owns.lock.file()))
                .await?;
            MarkerState::Absent
        } else {
            marker
        };
        let status = status?;
        let owns = ownership.clone();
        let marker_identity = self
            .0
            .0
            .file_workers
            .run(move || publish_marker(&owns, status, &marker))
            .await?;
        Ok(RootLease {
            ownership,
            state: self.clone(),
            status,
            marker_identity,
            authority: Arc::new(super::authority::ControlAuthority::Permissive),
            stopping: None,
            tree_limits: Default::default(),
            published_observation: Default::default(),
        })
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
