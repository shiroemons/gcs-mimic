//! Root identity, exclusive ownership and fail-closed baseline activation.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{CatalogId, SyncState, catalog_id};
use crate::storage::catalog::sql;
use crate::storage::failure::{Link, TypedSource};
use crate::{
    bucket::CatalogError,
    fsutil::{self, FileIdentity, FsError, NodeId},
    storage::{
        admin::AdminError,
        lock,
        sync_state::{
            OpaqueId,
            deletions::{EmptiedAtStart, Seen},
        },
    },
    sync::{
        mapping::{MARKER, MARKER_STAGING},
        naming::RootNaming,
    },
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
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

impl Baseline {
    /// The baseline a `complete` flag stands for, as `sync_roots.complete` and a pass record it.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "converts the flag the catalog column and a pass's report store as a boolean"
    )]
    pub(crate) const fn from_complete(complete: bool) -> Self {
        if complete {
            Self::Complete
        } else {
            Self::Activating
        }
    }

    /// The baseline the catalog holds for `root` now, read through `connection`.
    pub(super) fn read(connection: &Connection, root: RootId) -> rusqlite::Result<Self> {
        sql::scalar(
            connection,
            "SELECT complete FROM sync_roots WHERE root_id=?1",
            [root],
        )
        .map(Self::from_complete)
    }
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
    /// The selected directory has no marker and holds more than files directly in it, and the
    /// data directory synchronizes the directories at these paths.
    ReplacementOccupied {
        synchronized: Vec<String>,
    },
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
            Self::ReplacementOccupied { .. } => f.write_str("ReplacementOccupied"),
        }
    }
}
impl std::error::Error for RootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for RootError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Admin(error) => Some(Link::Admin(error)),
            Self::InUse
            | Self::Unsupported
            | Self::RootChanged
            | Self::MissingMarker
            | Self::InvalidMarker
            | Self::ForeignMarker
            | Self::ReinitializeDenied
            | Self::ReplacementOccupied { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    catalog_id: CatalogId,
    root_id: RootId,
}

impl Marker {
    fn new(status: RootStatus) -> Self {
        Self {
            version: 1,
            catalog_id: status.catalog_id,
            root_id: status.root_id,
        }
    }

    fn owned_by(&self, status: RootStatus) -> bool {
        self.catalog_id == status.catalog_id && self.root_id == status.root_id
    }
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
    if marker.version != 1 {
        return Err(RootError::InvalidMarker);
    }
    let current = fsutil::open_regular_at(directory, name.as_ref()).map_err(marker_open_error)?;
    if fsutil::stat_identity(&current)? != before {
        return Err(RootError::InvalidMarker);
    }
    Ok(Some((marker, before)))
}

#[derive(Clone, Copy)]
enum MarkerState {
    Published(Marker, FileIdentity),
    Staged(Marker),
    Absent,
}
impl MarkerState {
    fn marker(&self) -> Option<Marker> {
        match self {
            Self::Published(marker, _) | Self::Staged(marker) => Some(*marker),
            Self::Absent => None,
        }
    }
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

/// Writes `marker` under the staging name and syncs it and the root, so that a crash from here
/// on leaves a complete staged marker, which a restart publishes while the catalog registers
/// its root at this directory and removes otherwise.
fn stage_marker(owns: &Ownership, marker: Marker) -> Result<switch::Staged, RootError> {
    let root = owns.lock.file();
    remove_staging(root)?;
    let bytes = serde_json::to_vec(&marker).map_err(AdminError::backend)?;
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
    Ok(switch::Staged(marker))
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
        stage_marker(owns, Marker::new(status))?;
    }
    if !matches!(marker, MarkerState::Published(..)) {
        #[cfg(test)]
        super::super::fault::hit_io(&owns.path, super::super::fault::Step::SyncMarkerRename)?;
        fsutil::durable_replace_between(root, MARKER_STAGING.as_ref(), root, MARKER.as_ref(), None)
            .map_err(|error| match error {
                FsError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    RootError::ForeignMarker
                }
                error @ (FsError::Io(_) | FsError::Unsupported) => error.into(),
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
    if !current.owned_by(status) {
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
    /// Set once the mirror has probed the pinned root, and shared by every clone of its lease.
    naming: OnceLock<RootNaming>,
    /// What the deletion inventories of this process found of the buckets emptied while the
    /// server was stopped; `None` until the first of them completed.
    emptied_at_start: Mutex<Option<EmptiedAtStart>>,
    /// What the last inventory of this process that completed the root's set saw of it, keyed
    /// by the set's revision.
    waiting: Mutex<Option<(OpaqueId, Seen)>>,
    /// Whether the root may hold alias findings, shared by every clone of its lease.
    aliases: super::findings::alias::AliasPresence,
    _catalog_permit: OwnedSemaphorePermit,
}

/// What startup registers for the selected directory.
enum Reservation {
    /// The selected directory is the root `status` names. `change` is what startup changed to
    /// register it, when the data directory synchronized another directory before.
    Reserved {
        status: RootStatus,
        change: Option<Box<switch::Change>>,
    },
    /// A directory without a marker in place of the registered roots, which startup switches to
    /// only when it holds nothing but files directly in it.
    Replaced(switch::Replaced),
}

/// Whose marker the selected directory holds.
#[derive(Clone, Copy)]
enum Mark {
    Absent,
    /// This data directory's marker, for the root it names.
    Own(RootId),
    /// Another data directory's.
    Foreign,
}

/// What startup does with the selected directory, by the rules [`switch`] describes.
enum Decision {
    /// Opens the registered root as it is, with its tracked files and its baseline.
    Open { root: RootId, complete: bool },
    /// Builds the baseline of the registered root again, keeping its tracked files.
    Reinitialize(RootId),
    /// Registers the data directory's first root.
    First,
    /// Changes the synchronized directory to the selected one under the root its marker names,
    /// or that `--sync-reinitialize` named, forgetting every registered root and tracked file.
    Rebind {
        reason: switch::Reason,
        root: RootId,
    },
    /// Switches to the selected directory under a new root when it holds only root files.
    Replace,
}

/// The one place that decides how startup takes the selected directory, at `location` with the
/// identity `node`.
fn decide(
    registered: &[switch::Registered],
    location: &[u8],
    node: NodeId,
    mark: Mark,
    catalog: CatalogId,
    activation: Activation,
) -> Result<Decision, RootError> {
    let at = registered
        .iter()
        .find(|root| root.location == location && root.node == node);
    let near = registered
        .iter()
        .any(|root| root.location == location || root.node == node);
    let alone = registered.len() == 1;
    match (activation, mark, at) {
        (_, Mark::Foreign, _) => Err(RootError::ForeignMarker),
        (Activation::Open, Mark::Own(id), Some(root)) if id == root.root_id && alone => {
            Ok(Decision::Open {
                root: id,
                complete: root.complete,
            })
        }
        (Activation::Open, Mark::Own(id), Some(root)) if id == root.root_id => {
            Ok(Decision::Rebind {
                reason: switch::Reason::SeveralRoots,
                root: id,
            })
        }
        (Activation::Open, Mark::Own(id), _) => Ok(Decision::Rebind {
            reason: switch::Reason::MarkedDirectory,
            root: id,
        }),
        (Activation::Open, Mark::Absent, Some(_)) => Err(RootError::MissingMarker),
        (Activation::Open, Mark::Absent, None) if registered.is_empty() => Ok(Decision::First),
        (Activation::Open, Mark::Absent, None) => Ok(Decision::Replace),
        (Activation::Reinitialize { catalog_id }, Mark::Absent, Some(root))
            if catalog_id == catalog && alone =>
        {
            Ok(Decision::Reinitialize(root.root_id))
        }
        (Activation::Reinitialize { catalog_id }, Mark::Absent, Some(root))
            if catalog_id == catalog =>
        {
            Ok(Decision::Rebind {
                reason: switch::Reason::SeveralRoots,
                root: root.root_id,
            })
        }
        (Activation::Reinitialize { .. }, Mark::Absent, None) if near => {
            Err(RootError::RootChanged)
        }
        (Activation::Reinitialize { .. }, Mark::Absent | Mark::Own(_), _) => {
            Err(RootError::ReinitializeDenied)
        }
    }
}

fn reserve(
    connection: &mut Connection,
    location: &[u8],
    identity: FileIdentity,
    marker: Option<Marker>,
    catalog: CatalogId,
    activation: Activation,
) -> Result<Reservation, RootError> {
    let transaction = connection
        .transaction()
        .map_err(super::super::unavailable)?;
    if catalog_id(&transaction)? != catalog {
        return Err(RootError::ForeignMarker);
    }
    let node = identity.node();
    let registered = switch::registered(&transaction)?;
    let mark = match marker {
        None => Mark::Absent,
        Some(marker) if marker.catalog_id == catalog => Mark::Own(marker.root_id),
        Some(_) => Mark::Foreign,
    };
    let (root_id, baseline, change) =
        match decide(&registered, location, node, mark, catalog, activation)? {
            Decision::Open { root, complete } => (root, Baseline::from_complete(complete), None),
            Decision::Reinitialize(root) => {
                sql::exec(
                    &transaction,
                    "UPDATE sync_roots SET complete = 0 WHERE root_id = ?1",
                    [root.0],
                )
                .map_err(super::super::unavailable)?;
                (root, Baseline::Activating, None)
            }
            Decision::First => {
                let root = RootId::generate()
                    .map_err(|error| RootError::Admin(AdminError::backend(error)))?;
                insert_root(&transaction, root, location, node)?;
                (root, Baseline::Activating, None)
            }
            Decision::Rebind { reason, root } => {
                let change =
                    switch::rebind(&transaction, reason, registered, root, location, node)?;
                (root, Baseline::Activating, Some(Box::new(change)))
            }
            Decision::Replace => return Ok(Reservation::Replaced(switch::Replaced(registered))),
        };
    transaction.commit().map_err(super::super::unavailable)?;
    Ok(Reservation::Reserved {
        status: RootStatus {
            catalog_id: catalog,
            root_id,
            baseline,
        },
        change,
    })
}

/// Registers the directory at `location` with the identity `node` as the root `root_id`, with
/// its baseline still to build.
fn insert_root(
    transaction: &Connection,
    root_id: RootId,
    location: &[u8],
    node: NodeId,
) -> Result<(), RootError> {
    let (device, inode) = node.to_be_bytes();
    sql::exec(
        transaction,
        "INSERT INTO sync_roots VALUES (?1,?2,?3,?4,0)",
        params![root_id, location, device, inode],
    )
    .map_err(super::super::unavailable)?;
    Ok(())
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
            if identity.node() != self.identity.node() {
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
    /// What the last complete deletion inventory of this process kept of the buckets emptied
    /// while the server was stopped; `None` while none has completed, which makes the next one
    /// the first, whose findings went while the server was stopped.
    pub(crate) fn emptied_at_start(&self) -> Option<EmptiedAtStart> {
        self.ownership
            .emptied_at_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records what a complete deletion inventory kept of the buckets emptied at the start. A
    /// bucket it leaves out, because one of those files was there again, drops out for good, so
    /// that emptying it later is a deletion made while the mirror watched.
    pub(crate) fn keep_emptied_at_start(&self, kept: EmptiedAtStart) {
        *self
            .ownership
            .emptied_at_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(kept);
    }

    /// Records what an inventory saw of the set `revision`, which it just completed.
    pub(in crate::storage::catalog::sync_state) fn keep_seen(
        &self,
        revision: OpaqueId,
        seen: Seen,
    ) {
        *self
            .ownership
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((revision, seen));
    }

    /// What an inventory of this process saw of the set `revision`; nothing for another set.
    pub(in crate::storage::catalog::sync_state) fn seen_waiting(&self, revision: OpaqueId) -> Seen {
        self.ownership
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(waiting, _)| *waiting == revision)
            .map_or_else(Seen::default, |(_, seen)| seen.clone())
    }
    pub(crate) fn watch_path(&self) -> &Path {
        &self.ownership.path
    }
    /// How this process changed the synchronized directory at startup, if it did.
    pub(crate) fn change(&self) -> Option<&RootChange> {
        self.change.as_deref()
    }
    pub(crate) fn observation_time_ns(&self) -> i128 {
        crate::clock::signed_nanos(self.catalog().0.clock.now())
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
    change: Option<Arc<RootChange>>,
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
    /// The naming rules probed for this root. Mapping names before the probe is a bug, so it
    /// fails instead of assuming rules.
    pub(crate) fn naming(&self) -> Result<RootNaming, AdminError> {
        self.ownership.naming.get().copied().ok_or_else(|| {
            AdminError::Backend("synchronization root naming rules were not probed".into())
        })
    }
    pub(super) fn alias_presence(&self) -> &super::findings::alias::AliasPresence {
        &self.ownership.aliases
    }
    /// Lookups below the pinned root under its probed naming rules, for one pass: they keep the
    /// directory listings they read until the pass drops them.
    pub(crate) fn lookup(&self) -> Result<crate::fsutil::Lookup, AdminError> {
        Ok(crate::fsutil::Lookup::probed(
            self.naming()?.matching(),
            Arc::clone(&self.catalog().0.clock),
        ))
    }
    /// Records the probed naming rules. A later probe of the same pinned root that observes other
    /// rules means its filesystem changed underneath it.
    pub(crate) fn record_naming(&self, naming: RootNaming) -> Result<(), RootError> {
        if *self.ownership.naming.get_or_init(|| naming) != naming {
            return Err(RootError::RootChanged);
        }
        Ok(())
    }
    /// Records rules without a probe, for tests that drive the catalog below the mirror.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn assume_naming(self, naming: RootNaming) -> Self {
        self.record_naming(naming)
            .unwrap_or_else(|error| panic!("conflicting assumed naming rules: {error}"));
        self
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
        if !marker.owned_by(self.status) {
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
                        naming: OnceLock::new(),
                        emptied_at_start: Mutex::new(None),
                        waiting: Mutex::new(None),
                        aliases: super::findings::alias::AliasPresence::default(),
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
        let (reservation,staging_valid) = self
            .0
            .run_admin_with_error(move |connection| {
                let valid=match marker {
                    MarkerState::Staged(staged) => {
                        let (device,inode)=identity.node().to_be_bytes();
                        staged.catalog_id==catalog && sql::scalar::<bool>(connection,"SELECT EXISTS(SELECT 1 FROM sync_roots WHERE location=?1 AND device=?2 AND inode=?3 AND root_id=?4)",params![location,device,inode,staged.root_id]).map_err(super::super::unavailable)?
                    }
                    MarkerState::Published(..) | MarkerState::Absent => true,
                };
                Ok::<_,RootError>((reserve(connection,&location,identity,marker.marker().filter(|_| valid),catalog,activation),valid))
            })
            .await?;
        let marker = if matches!(marker, MarkerState::Staged(_)) && !staging_valid {
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
        let (status, marker, change) = match reservation? {
            Reservation::Reserved { status, change } => (status, marker, change),
            Reservation::Replaced(replaced) => {
                let (status, marker, change) =
                    self.switch_root(&ownership, catalog, replaced).await?;
                (status, marker, Some(Box::new(change)))
            }
        };
        // The change committed whether or not publishing the marker succeeds.
        let change = change.map(|change| {
            change.report();
            Arc::new(change.summary())
        });
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
            change,
        })
    }
}

mod switch;
pub(crate) use switch::RootChange;

#[cfg(test)]
#[cfg(unix)]
mod tests;
