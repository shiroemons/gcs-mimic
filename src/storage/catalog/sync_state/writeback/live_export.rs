use super::super::SyncRecord;
use super::*;
use std::ops::ControlFlow;

mod directory;
#[cfg(test)]
pub(super) mod faults;
mod owned;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) mod tests;
#[cfg(test)]
use crate::storage::catalog::fault::Step;
pub(super) use owned::OwnedBody;

/// One armed failure: the root and phase it fires at, the hits it lets pass first, how many
/// hits after those it fails, and the kind of I/O failure each returns.
#[cfg(test)]
struct Armed {
    root: RootId,
    phase: &'static str,
    skip: usize,
    count: usize,
    kind: std::io::ErrorKind,
}

#[cfg(test)]
static FAILURE: std::sync::Mutex<Vec<Armed>> = std::sync::Mutex::new(Vec::new());

/// Fails `count` hits of `phase` for `root` as a full disk.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject_failure(root: RootId, phase: &'static str, count: usize) {
    inject_failure_after(root, phase, 0, count);
}

/// Fails `count` hits of `phase` for `root` with an I/O failure of `kind`, such as one that
/// concerns the target path alone rather than the whole file system.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject_io_failure(
    root: RootId,
    phase: &'static str,
    count: usize,
    kind: std::io::ErrorKind,
) {
    arm(Armed {
        root,
        phase,
        skip: 0,
        count,
        kind,
    });
}

/// Fails `count` hits of `phase` for `root` as a full disk after letting the next `skip` pass,
/// so that a test can fail a later object's export while an earlier one completes.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject_failure_after(root: RootId, phase: &'static str, skip: usize, count: usize) {
    arm(Armed {
        root,
        phase,
        skip,
        count,
        kind: std::io::ErrorKind::StorageFull,
    });
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn arm(armed: Armed) {
    FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(armed);
}

/// Makes the next `count` staged copies of `root` record an inode number that no later
/// observation repeats. Docker Desktop's shared folders can report one number for a file just
/// created and another for it later; this reproduces that without them.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject_renumbered_copy(root: RootId, count: usize) {
    inject_failure(root, RENUMBERED_COPY, count);
}

#[cfg(test)]
const RENUMBERED_COPY: &str = "renumbered-copy";

/// The kind of failure a test armed `phase` for `root` with, if it did; each armed count fires
/// once, after the hits it skips.
#[cfg(test)]
fn armed(root: RootId, phase: &str) -> Result<Option<std::io::ErrorKind>, Error> {
    let mut failure = FAILURE
        .lock()
        .map_err(|_| AdminError::Backend("live-export failure injection poisoned".into()))?;
    let Some(armed) = failure
        .iter_mut()
        .find(|armed| armed.root == root && armed.phase == phase && armed.count > 0)
    else {
        return Ok(None);
    };
    if armed.skip > 0 {
        armed.skip -= 1;
        return Ok(None);
    }
    armed.count -= 1;
    Ok(Some(armed.kind))
}

#[cfg(test)]
fn failure(root: RootId, phase: &str) -> Result<(), Error> {
    match armed(root, phase)? {
        Some(kind) => Err(AdminError::from(std::io::Error::from(kind)).into()),
        None => Ok(()),
    }
}

#[cfg(test)]
fn renumbered(root: RootId, identity: fsutil::FileIdentity) -> Result<fsutil::FileIdentity, Error> {
    Ok(if armed(root, RENUMBERED_COPY)?.is_some() {
        fsutil::FileIdentity {
            inode: !identity.inode,
            ..identity
        }
    } else {
        identity
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    Pending,
    Complete,
    Conflict,
    Obsolete,
    /// The tree changed under an installation attempt in a way the attempt checks for, such as
    /// a directory replaced at the target or on the path to it after the attempt read it.
    /// Nothing was installed, and the operation is cancelled with its private copy: a new export
    /// plans again from what the tree holds then, through every check this one ran.
    Raced,
    /// A directory holds the target's name and stays: it holds an entry, the catalog refers to
    /// it or to a path below it, or the export had a body there to replace. The reason is the
    /// finding that keeps the object out of the tree.
    Collision(mapping::Obstruction),
}

/// Whether an installation attempt failed because the tree changed under it. Its checks fail
/// with [`stale`] where they find the tree other than the attempt read it moments before: the
/// directory at the target replaced, the path to the target no longer resolving, or an alias in
/// the place of an entry. That is a user's concurrent change, which a new export plans around,
/// not a failure of the root; pausing the whole root over it would leave every other path
/// unsynchronized until an operator rescans.
fn raced(error: &Error) -> bool {
    matches!(error, Error::Admin(AdminError::Precondition))
}

/// The regular file at an export's target, where a failed or cancelled attempt may have left its
/// copy. A directory at the target holds no copy, and a path that no longer resolves
/// ([`raced`]) holds nothing the export can reach: a copy installed before the user changed the
/// path went with the directory they moved, as any file of theirs does.
fn target_file(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &str,
) -> Result<Option<File>, Error> {
    match target_entry(root, naming, lookup, path) {
        Ok(TargetEntry::File(file)) => Ok(Some(file)),
        Ok(TargetEntry::Directory | TargetEntry::Missing) => Ok(None),
        Err(error) if raced(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// How one installation attempt ended.
#[derive(Debug)]
enum Installed {
    /// The target holds the exported state.
    Done,
    /// The target changed since the export began.
    Changed,
    /// A directory holds the target's name, and the attempt had no proof to remove it with.
    Directory,
    /// A directory holds the target's name and stays.
    Kept(directory::Kept),
}

#[derive(Clone)]
pub(crate) struct LiveExport(Arc<tokio::sync::Mutex<LiveOperation>>);

pub(crate) enum LiveSource {
    Observe,
    ApiCopy(File),
    ProtectedRestore(File),
    ProtectedObservation,
    Preserved {
        source: Option<File>,
        conflict: Box<super::super::conflicts::CapturedConflict>,
    },
}

impl LiveSource {
    fn into_parts(self) -> (Purpose, Option<File>) {
        match self {
            Self::Observe => (Purpose::Ordinary, None),
            Self::ApiCopy(file) => (Purpose::Ordinary, Some(file)),
            Self::ProtectedRestore(file) => (Purpose::RestoreAbsent, Some(file)),
            Self::ProtectedObservation => (Purpose::ObserveProtected, None),
            Self::Preserved { source, conflict } => (Purpose::Preserved(conflict), source),
        }
    }
}

/// What an export writes, fixed when it begins.
enum Body {
    /// Copy this API body into the workspace, then install it over the target.
    Copy {
        source: File,
        record: crate::object::ObjectRecord,
    },
    /// The target must already hold the live body; nothing is copied or installed.
    Observe { record: crate::object::ObjectRecord },
    /// Move the tracked target into the workspace and unlink it.
    Remove,
}

impl Body {
    /// Publication needs the live record; a source without one means the API object changed.
    fn new(
        source: Option<File>,
        record: Option<crate::object::ObjectRecord>,
    ) -> Result<Self, Error> {
        match (source, record) {
            (Some(source), Some(record)) => Ok(Self::Copy { source, record }),
            (None, Some(record)) => Ok(Self::Observe { record }),
            (None, None) => Ok(Self::Remove),
            (Some(_), None) => Err(stale()),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Copy { .. } | Self::Observe { .. } => Kind::Publish,
            Self::Remove => Kind::Remove,
        }
    }

    /// An observation never copies, so it starts where a copy ends.
    fn first_step(&self) -> Progress {
        match self {
            Self::Observe { .. } => Progress::Staged(None),
            // `Remove` keeps its empty copy step: tests pin three `Pending` results and
            // callers may cancel between steps.
            Self::Copy { .. } | Self::Remove => Progress::Prepared,
        }
    }
}

/// Why the export may replace or remove a file, fixed when it begins.
#[derive(Clone)]
enum Purpose {
    /// An ordinary export; the original's holds are enforced.
    Ordinary,
    /// Restore a protected body into a target that must still be absent.
    RestoreAbsent,
    /// Confirm that a protected target already holds the live body.
    ObserveProtected,
    /// Export over a target whose previous body was first preserved as this conflict.
    Preserved(Box<super::super::conflicts::CapturedConflict>),
}

impl Purpose {
    fn conflict(&self) -> Option<&super::super::conflicts::CapturedConflict> {
        match self {
            Self::Preserved(conflict) => Some(conflict),
            Self::Ordinary | Self::RestoreAbsent | Self::ObserveProtected => None,
        }
    }

    /// Only a preserved original pins the device its replacement must stay on.
    fn source_device(&self) -> Option<u64> {
        self.conflict()
            .map(super::super::conflicts::CapturedConflict::device)
    }
}

/// How far an export has progressed; each `advance_live_export` call moves at most one step.
enum Progress {
    /// Nothing is in the workspace yet.
    Prepared,
    /// The workspace holds the durable private copy (`None` when the body copies nothing).
    Staged(Option<fsutil::FileIdentity>),
    /// The target holds the exported state; its fingerprint is not yet durable in SQL.
    Installed(Option<fsutil::FileIdentity>),
    /// Phase 3 is committed with this fingerprint (`None`: the target is absent).
    Durable {
        staged: Option<fsutil::FileIdentity>,
        fingerprint: Option<SyncFingerprint>,
    },
    /// An abandoned export reports `result` once its cleanup commits; a failed cleanup retries.
    Cancelling {
        staged: Option<fsutil::FileIdentity>,
        result: InstallOutcome,
    },
}

impl Progress {
    fn staged(&self) -> Option<fsutil::FileIdentity> {
        match self {
            Self::Prepared => None,
            Self::Staged(staged)
            | Self::Installed(staged)
            | Self::Durable { staged, .. }
            | Self::Cancelling { staged, .. } => *staged,
        }
    }

    fn cancel(&self, result: InstallOutcome) -> Self {
        Self::Cancelling {
            staged: self.staged(),
            result,
        }
    }
}

struct LiveOperation {
    /// The `sync_writeback_ops` row as last read; its phase and revision guard every step.
    item: Pending,
    /// The tracked body the target must still hold before it is replaced.
    tracked: Option<SyncRecord>,
    body: Body,
    purpose: Purpose,
    progress: Progress,
}

/// What the installation job needs; it runs on a file worker, so it owns its inputs.
struct Install {
    item: Pending,
    naming: RootNaming,
    lookup: fsutil::Lookup,
    tracked: Option<SyncRecord>,
    /// The staged copy's identity; installation replaces it with the one it confirmed.
    staged: Option<fsutil::FileIdentity>,
    purpose: Purpose,
    /// Where the fault model records the namespace changes installation makes.
    #[cfg(test)]
    faults: faults::Paths,
}

/// Whether `file` still holds the tracked body, by [`body::same_content`] with the recorded
/// modification time. Installation and restart recovery both decide with this, so recovery
/// removes no tracked body that installation would have kept.
fn matches_tracked(file: &File, tracked: &SyncRecord) -> Result<body::Match, Error> {
    let fingerprint = &tracked.fingerprint;
    body::same_content(file, fingerprint.into(), Some(fingerprint.mtime_ns))
}

/// The step that checks a private copy against its staged identity.
#[derive(Clone, Copy)]
enum CopyCheck {
    Install,
    Cancel,
}

impl CopyCheck {
    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Cancel => "cancel",
        }
    }
}

/// Confirms that the private copy `file` is still the copy staged as `staged`
/// ([`body::own_copy`]), and returns the identity to compare it with from now on. Anything else
/// is `Protected`, logged with the changed fields and whether the bytes were checked.
fn confirm_copy(
    file: &File,
    staged: fsutil::FileIdentity,
    item: &Pending,
    check: CopyCheck,
) -> Result<fsutil::FileIdentity, Error> {
    let (changes, content) = match body::own_copy(file, staged, item.into())? {
        body::CopyMatch::Same(identity) => return Ok(identity),
        body::CopyMatch::Differs { changes, content } => (changes, content),
    };
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "sync_writeback_copy_changed",
        serde_json::json!({
            "path": item.path,
            "operation": item.token.id.to_hex(),
            "check": check.as_str(),
            "changed": changes.iter().map(fsutil::IdentityField::as_str).collect::<Vec<_>>(),
            "content": content.as_str(),
        }),
    );
    Err(Error::Protected)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdentityStage {
    BeforeExchange,
    Displaced,
}
fn matches_source(
    file: &File,
    expected: &SyncRecord,
    device: Option<u64>,
    stage: IdentityStage,
) -> Result<body::Match, Error> {
    if let Some(device) = device {
        let identity = fsutil::stat_identity(file)?;
        // Rename changes ctime; the pre-exchange capture must still match every original observation.
        if identity.device != device
            || (stage == IdentityStage::BeforeExchange
                && identity.changed_ns != expected.fingerprint.ctime_ns)
        {
            return Ok(body::Match::Differs);
        }
    }
    matches_tracked(file, expected)
}

fn api_original(c: &Transaction<'_>, item: &Pending, purpose: &Purpose) -> Result<(), Error> {
    validate_capture(c, item, purpose)?;
    if item.kind == Kind::Publish {
        return match purpose {
            Purpose::Ordinary => original(c, item),
            Purpose::RestoreAbsent | Purpose::Preserved(_) | Purpose::ObserveProtected => {
                original_with_protection(c, item, OriginalProtection::ReadOnlyCopy)
            }
        };
    }
    if matches!(purpose, Purpose::RestoreAbsent | Purpose::ObserveProtected) {
        return Err(stale());
    }
    // An API deletion does not wait for a complete baseline. That guard keeps external
    // deletions from reaching the API; this removal follows the API and touches only a file
    // tracked at the deleted generation, and a locally edited file is kept as a conflict. Waiting
    // here also deadlocked: the pending removal kept the baseline from ever completing.
    let binding = SyncBinding::new(item.token.catalog, item.bucket.clone(), item.incarnation);
    super::super::matching(c, &binding, item.bucket.as_str())?;
    if objects::lookup(c, item.bucket.as_str(), item.name.as_str())?.is_some() {
        return Err(stale());
    }
    let valid: bool = c.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4)")?.query_row(params![item.bucket,item.name,item.incarnation,item.generation], |r| r.get(0))?;
    if !valid {
        return Err(stale());
    }
    Ok(())
}

fn validate_capture(tx: &Transaction<'_>, item: &Pending, purpose: &Purpose) -> Result<(), Error> {
    if let Purpose::Preserved(conflict) = purpose {
        let binding = SyncBinding::new(item.token.catalog, item.bucket.clone(), item.incarnation);
        conflict.validate(
            tx,
            item.token.root,
            &binding,
            &item.name.clone(),
            if item.kind == Kind::Publish {
                Some(item.generation)
            } else {
                None
            },
        )?;
    }
    Ok(())
}

/// Whether the target already holds the live body an observation expects.
fn observe_target(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    item: &Pending,
) -> Result<bool, Error> {
    match target(root, naming, lookup, &item.path)? {
        Some(file) => Ok(body::same_content(&file, item.into(), None)?
            .same()
            .is_some()),
        None => Ok(false),
    }
}

/// The directory that holds `item`'s target. A publication creates the directories its target
/// needs. A removal only opens them: when one is missing, so is the target, and creating it would
/// put back a directory that someone removed. `None` is a removal's missing directory.
fn target_directory(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    item: &Pending,
) -> Result<Option<File>, Error> {
    match item.kind {
        Kind::Publish => {
            let parents = item.path.rsplit_once('/').ok_or_else(stale)?.0;
            Ok(Some(crate::sync::tree::parent(
                root,
                naming,
                lookup,
                std::path::Path::new(parents),
                crate::sync::tree::ParentOpen::Create,
            )?))
        }
        Kind::Remove => parent(root, naming, lookup, &item.path, ParentSync::Skip),
    }
}

/// What holds an installation's target name before it installs, with the directory that takes
/// the name.
enum Current {
    /// The regular file at the target, if any, and the directory that holds the target; `None`
    /// is a removal's missing directory.
    File {
        file: Option<File>,
        directory: Option<File>,
    },
    /// A directory that the installation may remove if it is empty. The proof holds the
    /// directory it read that entry in, and installation renames into that same handle, so the
    /// name the copy takes is the one the proof checked.
    Directory(directory::RemovableDirectory),
}

/// What holds `install`'s target name. A directory at a publication's target breaks with
/// [`Installed::Directory`] unless `removal` brings the catalog's half of the proof that it may
/// go, and with [`Installed::Kept`] when the tree's half fails. A removal's target keeps failing
/// on a directory, as it did before directories were removed: nothing takes its place.
fn current_target(
    root: &File,
    install: &Install,
    removal: Option<directory::Unreferenced>,
) -> Result<ControlFlow<Installed, Current>, Error> {
    let selected = &install.item;
    let file = if selected.kind == Kind::Remove {
        target(root, install.naming, &install.lookup, &selected.path)?
    } else {
        match target_entry(root, install.naming, &install.lookup, &selected.path)? {
            TargetEntry::File(file) => Some(file),
            TargetEntry::Missing => None,
            TargetEntry::Directory => {
                let Some(removal) = removal else {
                    return Ok(ControlFlow::Break(Installed::Directory));
                };
                return Ok(
                    match directory::RemovableDirectory::prove(
                        root,
                        install.naming,
                        &install.lookup,
                        removal,
                    )? {
                        Ok(proof) => ControlFlow::Continue(Current::Directory(proof)),
                        Err(kept) => ControlFlow::Break(Installed::Kept(kept)),
                    },
                );
            }
        }
    };
    let directory = target_directory(root, install.naming, &install.lookup, selected)?;
    Ok(ControlFlow::Continue(Current::File { file, directory }))
}

/// Installs the staged copy at the target path (or moves a removed file into the workspace)
/// unless an earlier attempt already did. With `removal`, an empty directory at a publication's
/// target is removed just before the copy takes its name; [`current_target`] says when.
fn install_target(
    root: &File,
    workspace: &File,
    install: &mut Install,
    removal: Option<directory::Unreferenced>,
) -> Result<Installed, Error> {
    let current = match current_target(root, install, removal)? {
        ControlFlow::Continue(current) => current,
        ControlFlow::Break(ended) => return Ok(ended),
    };
    let retained = entry(workspace, install.item.token.id)?;
    let directory = match current {
        Current::File { file, directory } => {
            if !already_installed(install.staged, file.as_ref(), retained.as_ref())? {
                let identity = match expected_target(install, file.as_ref(), retained.as_ref())? {
                    ControlFlow::Continue(identity) => identity,
                    ControlFlow::Break(ended) => return Ok(ended),
                };
                if (file.is_some() || install.item.kind == Kind::Publish)
                    && let ControlFlow::Break(ended) =
                        rename(workspace, install, directory.as_ref(), identity.as_ref())?
                {
                    return Ok(ended);
                }
            }
            directory
        }
        // A directory at the target holds no installed copy and no file the export replaces. It
        // goes only after every other check, and the directory that held it takes the copy.
        Current::Directory(proof) => {
            if let ControlFlow::Break(ended) = expected_target(install, None, retained.as_ref())? {
                return Ok(ended);
            }
            let parent = match directory::remove(proof, install)? {
                Ok(parent) => parent,
                Err(kept) => return Ok(Installed::Kept(kept)),
            };
            if let ControlFlow::Break(ended) = rename(workspace, install, Some(&parent), None)? {
                return Ok(ended);
            }
            Some(parent)
        }
    };
    settle(root, workspace, install, directory.as_ref())
}

/// Whether an earlier attempt installed already: the target holds the `staged` copy, or, for a
/// removal, which stages nothing, the private entry holds the body it moved out.
fn already_installed(
    staged: Option<fsutil::FileIdentity>,
    current: Option<&File>,
    retained: Option<&File>,
) -> Result<bool, Error> {
    let Some(staged) = staged else {
        return Ok(retained.is_some());
    };
    Ok(current
        .map(fsutil::stat_identity)
        .transpose()?
        .is_some_and(|current| current.node() == staged.node()))
}

/// Checks that the target, holding `file`, still holds what the export expects and that the
/// `retained` private copy is still the staged one, before anything moves. Continues with the
/// identity of the tracked body the copy replaces, if any.
fn expected_target(
    install: &mut Install,
    file: Option<&File>,
    retained: Option<&File>,
) -> Result<ControlFlow<Installed, Option<fsutil::FileIdentity>>, Error> {
    if matches!(install.purpose, Purpose::RestoreAbsent) && file.is_some() {
        return Ok(ControlFlow::Break(Installed::Changed));
    }
    if let Some(staged) = install.staged {
        let copy = retained.ok_or(Error::Protected)?;
        install.staged = Some(confirm_copy(
            copy,
            staged,
            &install.item,
            CopyCheck::Install,
        )?);
    }
    let identity = match (file, install.tracked.as_ref()) {
        (None, _) => None,
        (Some(file), Some(expected)) => {
            let device = install.purpose.source_device();
            let Some(identity) =
                matches_source(file, expected, device, IdentityStage::BeforeExchange)?.same()
            else {
                return Ok(ControlFlow::Break(Installed::Changed));
            };
            Some(identity)
        }
        (Some(_), None) => return Ok(ControlFlow::Break(Installed::Changed)),
    };
    Ok(ControlFlow::Continue(identity))
}

/// Moves the staged copy into the target in `directory` (or a removed file out of it), exchanging
/// it with the tracked body of `identity` when there is one. A copy for an empty target ends
/// as [`Installed::Changed`] when something took the target after every check of it: the
/// rename does not replace it, and the next attempt would find it changed all the same.
fn rename(
    workspace: &File,
    install: &Install,
    directory: Option<&File>,
    identity: Option<&fsutil::FileIdentity>,
) -> Result<ControlFlow<Installed>, Error> {
    let selected = &install.item;
    let target = directory.ok_or_else(stale)?;
    let name = selected.path.rsplit('/').next().ok_or_else(stale)?;
    #[cfg(test)]
    install.faults.hit(Step::SyncCopyRename)?;
    let outcome = match selected.kind {
        Kind::Publish => match fsutil::durable_replace_between(
            workspace,
            selected.token.id.to_hex().as_ref(),
            target,
            name.as_ref(),
            identity,
        ) {
            Err(fsutil::FsError::Io(error))
                if identity.is_none() && error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                return Ok(ControlFlow::Break(Installed::Changed));
            }
            outcome => outcome?,
        },
        Kind::Remove => fsutil::durable_replace_between(
            target,
            name.as_ref(),
            workspace,
            selected.token.id.to_hex().as_ref(),
            None,
        )?,
    };
    #[cfg(test)]
    install.faults.renamed()?;
    if matches!(outcome, fsutil::ReplaceOutcome::Conflict { .. }) {
        return Err(Error::Protected);
    }
    Ok(ControlFlow::Continue(()))
}

/// Unlinks the tracked body that installation moved out of the target, if any, then syncs what
/// installation changed: the directory that holds the target, the workspace and the tree's root.
fn settle(
    root: &File,
    workspace: &File,
    install: &Install,
    directory: Option<&File>,
) -> Result<Installed, Error> {
    let selected = &install.item;
    #[cfg(test)]
    failure(selected.token.root, "installed")?;
    if let Some(file) = entry(workspace, selected.token.id)? {
        let device = install.purpose.source_device();
        let displaced = install.tracked.as_ref().map_or(Ok(None), |expected| {
            OwnedBody::displaced(&file, expected, device)
        })?;
        let Some(displaced) = displaced else {
            return Err(Error::Protected);
        };
        #[cfg(test)]
        failure(selected.token.root, "displaced-unlink")?;
        if !displaced.unlink(
            workspace,
            selected,
            #[cfg(test)]
            &install.faults,
        )? {
            return Err(Error::Protected);
        }
    }
    #[cfg(test)]
    failure(selected.token.root, "displaced-parent-sync")?;
    if let Some(directory) = directory {
        directory.sync_all()?;
        #[cfg(test)]
        install.faults.target_dir_synced(selected);
    }
    workspace.sync_all()?;
    #[cfg(test)]
    install.faults.workspace_synced();
    root.sync_all()?;
    #[cfg(test)]
    install.faults.tree_synced();
    Ok(Installed::Done)
}

impl Writeback {
    /// A missing source observes an identical live body, or removes an API-deleted tracked body.
    pub(crate) async fn begin_live_export(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
        tracked: Option<SyncRecord>,
        fence: JournalFence,
        source: LiveSource,
    ) -> Result<LiveExport, Error> {
        self.serial_with(move |this| async move {
            let (purpose, source) = source.into_parts();
            if let Some(conflict) = purpose.conflict() {
                conflict.verify_owned(&this.lease).await?;
            }
            let bucket = binding.bucket.clone();
            let path = match mapping::name_to_path(&bucket, &name, this.lease.naming()?) { PathMapping::File(p) => p.to_str().ok_or_else(stale)?.to_owned(), PathMapping::Unrepresentable(_) => return Err(stale()) };
            let (catalog, root) = this.identities();
            let selected = tracked.clone();
            let checked_purpose = purpose.clone();
            let (item,body,proof) = this.db(move |c| {
                #[cfg(test)]
                failure(root,"prepare")?;
                let tx = c.transaction()?;
                check_fence(&tx, fence)?;
                super::super::matching(&tx,&binding,bucket.as_str())?;
                let mut usage = status(&tx, root)?;
                if usage.stop() != Stop::Ready { return Err(Error::RecoveryRequired); }
                let (owner, incarnation): (String,Incarnation) = tx.prepare_cached("SELECT b.project,i.incarnation FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1")?.query_row([&bucket], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(stale)?;
                let stored = tx.prepare_cached(super::super::SELECT_BY_NAME)?.query_row(params![bucket,name,incarnation],super::super::read_record).optional()?;
                if stored != selected { return Err(stale()); }
                let record = objects::lookup(&tx,bucket.as_str(),name.as_str())?.map(|stored| stored.record);
                let expected_generation=record.as_ref().map(|record|Generation::new(record.generation)).transpose()?;
                let proof=checked_purpose.conflict().map(|conflict|conflict.validate(&tx,root,&binding,&name,expected_generation)).transpose()?;
                let body = Body::new(source, record)?;
                let (size,md5,crc32c) = match &body {
                    Body::Copy { record, .. } | Body::Observe { record } => (record.size,record.md5,record.crc32c),
                    Body::Remove => {
                        let record = selected.as_ref().ok_or_else(stale)?;
                        (record.fingerprint.size,record.fingerprint.md5,record.fingerprint.crc32c)
                    }
                };
                // A registered exact-body copy already owns its full retained charge.
                let reservation = if proof.is_some() { 0 } else { selected.as_ref().map_or(0,|s|s.fingerprint.size) };
                let captured_snapshot=checked_purpose.conflict().map(|conflict|conflict.durable_snapshot(&tx)).transpose()?;
                if if proof.is_some() {usage.over_limit()} else {usage.would_exceed(reservation)} { return Err(Error::Capacity); }
                let id = OpaqueId::generate().map_err(AdminError::backend)?;
                tx.prepare_cached("INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,1,?10,?11,?12,?13,?14,?15)")?.execute(params![id,root,owner,bucket,incarnation,name,path,generation,body.kind(),fence.sequence,fence.revision,reservation.to_be_bytes(),size,md5,crc32c])?;
                if let Some(snapshot)=captured_snapshot {
                    tx.prepare_cached("UPDATE sync_writeback_ops SET snapshot=?2 WHERE id=?1")?.execute(params![id,snapshot])?;
                }
                let item = load(&tx,catalog,root,id)?;
                api_original(&tx,&item,&checked_purpose)?;
                usage.entries += 1;
                usage.reserved_bytes += u128::from(reservation);
                usage.recovery = true;
                save_status(&tx,root,&usage)?;
                tx.commit()?;
                Ok((item,body,proof))
            }).await?;
            let tracked=proof.map_or(tracked,|proof|Some(SyncRecord {name:item.name.to_string(),path:item.path.clone(),generation:item.generation,fingerprint:proof.fingerprint}));
            let progress = body.first_step();
            Ok(LiveExport(Arc::new(tokio::sync::Mutex::new(LiveOperation { item, tracked, body, purpose, progress }))))
        }).await
    }

    /// Each call owns a complete file/SQL transition; callers may cancel only between calls.
    pub(crate) async fn advance_live_export(
        &self,
        operation: LiveExport,
    ) -> Result<InstallOutcome, Error> {
        self.serial_with(move |this| async move {
            let mut operation = operation.0.lock().await;
            let operation = &mut *operation;
            this.token(&operation.item.token)?;
            if operation.item.phase == Phase::Protected {
                return Err(Error::RecoveryRequired);
            }
            let selected = operation.item.token.clone();
            this.db(move |connection| {
                let current = checked(connection, &selected)?;
                let usage = status(connection, current.token.root)?;
                if current.phase == Phase::Protected
                    || usage.io_failure
                    || usage.protected_entries > 0
                    || usage.over_limit()
                {
                    return Err(Error::RecoveryRequired);
                }
                Ok(())
            })
            .await?;
            match &operation.progress {
                Progress::Cancelling { result, .. } => {
                    let result = *result;
                    this.cancel_live_copy(operation).await?;
                    Ok(result)
                }
                Progress::Prepared => {
                    this.copy_live(operation).await?;
                    Ok(InstallOutcome::Pending)
                }
                Progress::Staged(_) => this.install_live(operation).await,
                Progress::Installed(_) => this.record_live_durable(operation).await,
                Progress::Durable { fingerprint, .. } => {
                    let fingerprint = fingerprint.clone();
                    this.acknowledge_live(operation, fingerprint).await
                }
            }
        })
        .await
    }

    /// Copies the API body into a private workspace entry and makes it durable.
    async fn copy_live(&self, operation: &mut LiveOperation) -> Result<(), Error> {
        #[cfg(test)]
        let faults = self.fault_paths();
        let source = match &operation.body {
            Body::Copy { source, .. } => Some(source.try_clone()?),
            Body::Observe { .. } | Body::Remove => None,
        };
        let selected = operation.item.clone();
        let staged = self
            .fs(move |_, workspace| {
                let Some(source) = source else {
                    return Ok(None);
                };
                #[cfg(test)]
                failure(selected.token.root, "copy")?;
                // Only this unarmed operation can own this private, possibly partial API copy. A
                // retry refills it in place rather than removing and creating the name again;
                // `fsutil::clone_or_copy_into` says why, and `refillable_entry` names the one
                // macOS copy that cannot be refilled.
                let partial = refillable_entry(workspace, selected.token.id)?;
                if let Some(file) = &partial {
                    file.set_len(0)?;
                    #[cfg(test)]
                    faults.truncated(file)?;
                }
                #[cfg(test)]
                faults.hit(Step::SyncCopyWrite)?;
                if let Some(file) = &partial {
                    fsutil::clone_or_copy_onto(&source, file)?;
                } else {
                    fsutil::clone_or_copy_into(
                        &source,
                        workspace,
                        selected.token.id.to_hex().as_ref(),
                    )?;
                    #[cfg(test)]
                    faults.copy_created(&selected);
                }
                let file = entry(workspace, selected.token.id)?.ok_or_else(stale)?;
                if let body::Match::Differs = body::same_content(&file, (&selected).into(), None)? {
                    return Err(AdminError::Checksum.into());
                }
                #[cfg(test)]
                faults.hit(Step::SyncCopySync)?;
                file.sync_all()?;
                workspace.sync_all()?;
                #[cfg(test)]
                {
                    faults.synced(&file)?;
                    faults.workspace_synced();
                    faults.hit(Step::SyncCopyDurable)?;
                }
                let staged = fsutil::stat_identity(&file)?;
                #[cfg(test)]
                let staged = renumbered(selected.token.root, staged)?;
                Ok(Some(staged))
            })
            .await?;
        operation.progress = Progress::Staged(staged);
        Ok(())
    }

    /// Arms a prepared operation, then installs (or, when observing, checks) the target file.
    async fn install_live(&self, operation: &mut LiveOperation) -> Result<InstallOutcome, Error> {
        if let Some(conflict) = operation.purpose.conflict() {
            conflict.verify_owned(&self.lease).await?;
        }
        let Some(item) = self.arm_live(operation).await? else {
            return Ok(InstallOutcome::Obsolete);
        };
        let installed = match self.attempt_install(operation, &item, None).await {
            Ok(Installed::Directory) => match self.unreferenced_target(operation, &item).await? {
                Ok(removal) => self.attempt_install(operation, &item, Some(removal)).await,
                Err(kept) => Ok(Installed::Kept(kept)),
            },
            installed => installed,
        };
        let staged = operation.progress.staged();
        match installed {
            Ok(Installed::Changed) => {
                operation.progress = operation.progress.cancel(InstallOutcome::Conflict);
                self.cancel_live_copy(operation).await?;
                return Ok(InstallOutcome::Conflict);
            }
            Ok(Installed::Kept(kept)) => {
                kept.report(&item);
                return self.collide(operation, kept.finding()).await;
            }
            Ok(Installed::Directory) => {
                return self
                    .collide(operation, mapping::Obstruction::DirectoryOccupied)
                    .await;
            }
            Ok(Installed::Done) => operation.progress = Progress::Installed(staged),
            Err(Error::Protected) => {
                operation.item = self.inspect_inner(item.token.clone()).await?;
                return Err(Error::Protected);
            }
            Err(error) => {
                // Not on a file worker, whose root and workspace checks could replace `error`.
                #[cfg(test)]
                self.fault_paths().hit(Step::SyncCopyFailed)?;
                let selected = item.clone();
                let displaced = self.displaced_live(selected, staged).await?;
                if displaced {
                    let pending = self.inspect_inner(item.token.clone()).await?;
                    operation.item = pending.clone();
                    self.mark_entry_failure(pending.token).await?;
                    // Recovery owns displaced bytes; an automatic retry must not unlink them.
                    return Err(Error::Displaced(Box::new(error)));
                }
                if raced(&error) {
                    // The copy is not at the target, so cancelling unlinks only the private copy.
                    operation.progress = operation.progress.cancel(InstallOutcome::Raced);
                    self.cancel_live_copy(operation).await?;
                    return Ok(InstallOutcome::Raced);
                }
                return Err(error);
            }
        }
        Ok(InstallOutcome::Pending)
    }

    /// Cancels `operation`, whose target a directory holds, with `reason` as the finding that
    /// keeps its object out of the tree.
    async fn collide(
        &self,
        operation: &mut LiveOperation,
        reason: mapping::Obstruction,
    ) -> Result<InstallOutcome, Error> {
        let collision = InstallOutcome::Collision(reason);
        operation.progress = operation.progress.cancel(collision);
        self.cancel_live_copy(operation).await?;
        Ok(collision)
    }

    /// Arms `operation` unless it is armed already, and returns its row. `None` means the API
    /// object or the operation changed first, and the export was cancelled as obsolete.
    async fn arm_live(&self, operation: &mut LiveOperation) -> Result<Option<Pending>, Error> {
        if operation.item.phase != Phase::Prepared {
            return Ok(Some(operation.item.clone()));
        }
        let purpose = operation.purpose.clone();
        let selected = operation.item.clone();
        let armed = self
            .db(move |c| {
                #[cfg(test)]
                failure(selected.token.root, "arm")?;
                let tx = c.transaction()?;
                api_original(&tx, &selected, &purpose)?;
                checked(&tx, &selected.token)?;
                tx.prepare_cached(
                    "UPDATE sync_writeback_ops SET phase=1,revision=revision+1 WHERE id=?1",
                )?
                .execute([&selected.token.id])?;
                let armed = load(
                    &tx,
                    selected.token.catalog,
                    selected.token.root,
                    selected.token.id,
                )?;
                tx.commit()?;
                Ok(armed)
            })
            .await;
        match armed {
            Ok(armed) => {
                operation.item = armed.clone();
                Ok(Some(armed))
            }
            Err(Error::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                operation.progress = operation.progress.cancel(InstallOutcome::Obsolete);
                self.cancel_live_copy(operation).await?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Runs one installation attempt of the armed `item` on a file worker. The staged identity
    /// that installation confirmed is kept even when the attempt fails afterwards, so a retry, a
    /// cancellation and the displacement check compare against it.
    async fn attempt_install(
        &self,
        operation: &mut LiveOperation,
        item: &Pending,
        removal: Option<directory::Unreferenced>,
    ) -> Result<Installed, Error> {
        let staged = operation.progress.staged();
        let install = Install {
            item: item.clone(),
            naming: self.lease.naming()?,
            lookup: self.lease.lookup()?,
            tracked: operation.tracked.clone(),
            staged,
            purpose: operation.purpose.clone(),
            #[cfg(test)]
            faults: self.fault_paths(),
        };
        let observing = matches!(operation.body, Body::Observe { .. });
        let attempt = self
            .fs(move |root, workspace| {
                let mut install = install;
                #[cfg(test)]
                failure(install.item.token.root, "install")?;
                let installed = if observing {
                    observe_target(root, install.naming, &install.lookup, &install.item).map(
                        |holds| {
                            if holds {
                                Installed::Done
                            } else {
                                Installed::Changed
                            }
                        },
                    )
                } else {
                    install_target(root, workspace, &mut install, removal)
                };
                #[cfg(test)]
                let installed = installed.and_then(|installed| {
                    if matches!(installed, Installed::Done) && !observing {
                        install.faults.hit(Step::SyncCopyInstalled)?;
                    }
                    Ok(installed)
                });
                Ok((install.staged, installed))
            })
            .await;
        let (staged, installed) = attempt.unwrap_or_else(|error| (staged, Err(error)));
        operation.progress = Progress::Staged(staged);
        installed
    }

    /// The catalog's half of the proof that installation may remove the directory at `item`'s
    /// target, by [`directory::unreferenced`].
    async fn unreferenced_target(
        &self,
        operation: &LiveOperation,
        item: &Pending,
    ) -> Result<Result<directory::Unreferenced, directory::Kept>, Error> {
        let selected = item.clone();
        let tracked = operation.tracked.clone();
        let purpose = operation.purpose.clone();
        self.db(move |c| {
            // One read transaction, so the four reads see one state of the catalog.
            let tx = c.transaction()?;
            let unreferenced = directory::unreferenced(&tx, &selected, tracked.as_ref(), &purpose)?;
            tx.commit()?;
            Ok(unreferenced)
        })
        .await
    }

    /// Determines whether a failed installation has already displaced the target.
    async fn displaced_live(
        &self,
        item: Pending,
        staged: Option<fsutil::FileIdentity>,
    ) -> Result<bool, Error> {
        let naming = self.lease.naming()?;
        let lookup = self.lease.lookup()?;
        self.fs(move |root, workspace| {
            if let Some(staged) = staged {
                // Another file in the private copy's entry means the namespace already changed.
                // The copy itself under another inode number is not one: a failure before
                // installation confirmed the copy leaves `staged` as it was recorded.
                if let Some(retained) = entry(workspace, item.token.id)?
                    && fsutil::stat_identity(&retained)?.node() != staged.node()
                    && matches!(
                        body::own_copy(&retained, staged, (&item).into())?,
                        body::CopyMatch::Differs { .. }
                    )
                {
                    return Ok(true);
                }
                // Failing on what holds no installed copy, as opening a file there does, would
                // replace the error that ended the attempt.
                let current = target_file(root, naming, &lookup, &item.path)?
                    .map(|file| fsutil::stat_identity(&file))
                    .transpose()?;
                Ok(current.is_some_and(|current| current.node() == staged.node()))
            } else {
                Ok(item.kind == Kind::Remove && entry(workspace, item.token.id)?.is_some())
            }
        })
        .await
    }

    /// Records the installed file's fingerprint durably before acknowledging it.
    async fn record_live_durable(
        &self,
        operation: &mut LiveOperation,
    ) -> Result<InstallOutcome, Error> {
        if let Some(conflict) = operation.purpose.conflict() {
            conflict.verify_owned(&self.lease).await?;
        }
        let item = operation.item.clone();
        #[cfg(test)]
        failure(item.token.root, "durable")?;
        let selected = item.clone();
        let observed_ns = self.lease.observation_time_ns();
        let naming = self.lease.naming()?;
        let lookup = self.lease.lookup()?;
        let fp = match self
            .fs(move |root, workspace| {
                fingerprint(root, workspace, naming, &lookup, &selected, observed_ns)
            })
            .await
        {
            Ok(fp) => fp,
            Err(Error::Protected) => {
                operation.item = self.inspect_inner(item.token.clone()).await?;
                return Err(Error::Protected);
            }
            Err(Error::Admin(AdminError::Precondition | AdminError::NotFound))
                if matches!(operation.body, Body::Observe { .. }) =>
            {
                operation.progress = operation.progress.cancel(InstallOutcome::Conflict);
                self.cancel_live_copy(operation).await?;
                return Ok(InstallOutcome::Conflict);
            }
            Err(error) => return Err(error),
        };
        let snapshot = fp
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(AdminError::backend)?;
        let selected = item.clone();
        let purpose = operation.purpose.clone();
        let durable_fingerprint = fp.clone();
        operation.item = self.db(move |c| {
            let tx = c.transaction()?;
            checked(&tx,&selected.token)?;
            validate_capture(&tx,&selected,&purpose)?;
            let snapshot=if let Some(conflict)=purpose.conflict() {Some(conflict.durable_fingerprint(&tx,durable_fingerprint)?)} else {snapshot};
            tx.prepare_cached("UPDATE sync_writeback_ops SET phase=3,revision=revision+1,snapshot=?2 WHERE id=?1")?.execute(params![selected.token.id,snapshot])?;
            let durable = load(&tx,selected.token.catalog,selected.token.root,selected.token.id)?;
            tx.commit()?;
            Ok(durable)
        }).await?;
        operation.progress = Progress::Durable {
            staged: operation.progress.staged(),
            fingerprint: fp,
        };
        Ok(InstallOutcome::Pending)
    }

    /// Verifies the durable fingerprint still holds, then tracks it and finishes the operation.
    async fn acknowledge_live(
        &self,
        operation: &mut LiveOperation,
        fp: Option<SyncFingerprint>,
    ) -> Result<InstallOutcome, Error> {
        if let Some(conflict) = operation.purpose.conflict() {
            conflict.verify_owned(&self.lease).await?;
        }
        let purpose = operation.purpose.clone();
        let item = operation.item.clone();
        let selected = item.clone();
        let expected = fp.clone();
        let naming = self.lease.naming()?;
        let lookup = self.lease.lookup()?;
        let verified = self
            .fs(move |root, workspace| {
                verify_fingerprint(
                    root,
                    workspace,
                    naming,
                    &lookup,
                    &selected,
                    expected.as_ref(),
                )
            })
            .await;
        let durable = match verified {
            Ok(durable) => durable,
            Err(Error::Protected) => {
                operation.item = self.inspect_inner(item.token.clone()).await?;
                return Err(Error::Protected);
            }
            Err(Error::Admin(AdminError::Precondition | AdminError::NotFound))
                if matches!(operation.body, Body::Observe { .. }) =>
            {
                operation.progress = operation.progress.cancel(InstallOutcome::Conflict);
                self.cancel_live_copy(operation).await?;
                return Ok(InstallOutcome::Conflict);
            }
            Err(error) => return Err(error),
        };
        // `fingerprint` reports a body exactly when one was published, and `verify_fingerprint`
        // makes exactly a published one durable.
        let tracking = match (fp, durable, &operation.body) {
            (Some(fp), Some(durable), Body::Copy { record, .. } | Body::Observe { record }) => {
                Some((fp, durable, record.clone()))
            }
            (None, None, Body::Remove) => None,
            (_, _, Body::Copy { .. } | Body::Observe { .. } | Body::Remove) => {
                return Err(stale());
            }
        };
        let selected = item.clone();
        let scope = self.lease.control_scope();
        let result=self.db(move |c| {
            #[cfg(test)]
            failure(selected.token.root,"ack")?;
            let tx = c.transaction()?;
            let current = checked(&tx,&selected.token)?;
            let valid = match api_original(&tx,&current,&purpose) {
                Ok(()) => true,
                Err(Error::Admin(AdminError::Precondition | AdminError::NotFound)) => false,
                Err(error) => return Err(error),
            };
            let binding = SyncBinding::new(current.token.catalog, current.bucket.clone(), current.incarnation).with_control(scope);
            let same_binding = match super::super::matching(&tx,&binding,current.bucket.as_str()) {
                Ok(()) => true,
                Err(ObjectError::PreconditionFailed) => false,
                Err(error) => return Err(error.into()),
            };
            if same_binding {
                if let Some((fp, durable, record)) = tracking {
                    if durable.path().as_str() != current.path {
                        return Err(stale());
                    }
                    let update = SyncUpdate::new(binding,durable,fp)?;
                    // Track the bytes actually installed even if a newer API generation won meanwhile.
                    super::super::record_in_transaction(&tx,&update,&record)?;
                } else {
                    tx.prepare_cached("DELETE FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4")?.execute(params![current.bucket,current.name,current.incarnation,current.generation])?;
                    if valid {
                        tx.prepare_cached("DELETE FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND generation=?5")?.execute(params![current.token.root,current.bucket,current.name,current.incarnation,current.generation])?;
                    }
                }
            }
            finish_live(&tx,&current)?;
            tx.commit()?;
            Ok(if valid { InstallOutcome::Complete } else { InstallOutcome::Obsolete })
        }).await?;
        #[cfg(test)]
        self.fault_paths().hit(Step::SyncCopyCommit)?;
        Ok(result)
    }

    async fn cancel_live_copy(&self, operation: &mut LiveOperation) -> Result<(), Error> {
        let item = operation.item.clone();
        let selected = item.clone();
        let staged = operation.progress.staged();
        let naming = self.lease.naming()?;
        let lookup = self.lease.lookup()?;
        #[cfg(test)]
        let faults = self.fault_paths();
        let cancelled = self
            .fs(move |root, workspace| {
                // A directory that a cancelled export left at its target is not its copy, and
                // neither is anything at a path that no longer resolves.
                if let Some(staged) = staged
                    && let Some(file) = target_file(root, naming, &lookup, &selected.path)?
                    && fsutil::stat_identity(&file)?.node() == staged.node()
                {
                    return Err(Error::Protected);
                }
                if let Some(file) = entry(workspace, selected.token.id)? {
                    let owned = OwnedBody::staged(&file, &selected, staged)?;
                    if !owned.unlink(
                        workspace,
                        &selected,
                        #[cfg(test)]
                        &faults,
                    )? {
                        return Err(Error::Protected);
                    }
                    workspace.sync_all()?;
                    #[cfg(test)]
                    faults.workspace_synced();
                }
                Ok(())
            })
            .await;
        if matches!(cancelled, Err(Error::Protected)) {
            operation.item = self.inspect_inner(item.token.clone()).await?;
        }
        cancelled?;
        let selected = item.clone();
        self.db(move |c| {
            #[cfg(test)]
            failure(selected.token.root, "cancel")?;
            let tx = c.transaction()?;
            finish_live(&tx, &selected)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

pub(super) fn finish_live(tx: &Transaction<'_>, item: &Pending) -> Result<(), Error> {
    let mut usage = status(tx, item.token.root)?;
    usage.entries = usage.entries.checked_sub(1).ok_or_else(stale)?;
    usage.reserved_bytes = usage
        .reserved_bytes
        .checked_sub(u128::from(item.reserved_bytes))
        .ok_or_else(stale)?;
    usage.recovery = usage.entries != 0;
    save_status(tx, item.token.root, &usage)?;
    tx.prepare_cached("DELETE FROM sync_writeback_ops WHERE id=?1")?
        .execute([&item.token.id])?;
    Ok(())
}
