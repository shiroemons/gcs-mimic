use super::super::{SyncPath, SyncRecord};
use super::*;

#[cfg(test)]
pub(super) mod faults;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) mod tests;
#[cfg(test)]
use crate::storage::catalog::fault::Step;

#[cfg(test)]
static FAILURE: std::sync::Mutex<Vec<(RootId, &'static str, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject_failure(root: RootId, phase: &'static str, count: usize) {
    FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((root, phase, count));
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

/// Whether a test armed `phase` for `root`; each armed count fires once.
#[cfg(test)]
fn armed(root: RootId, phase: &str) -> Result<bool, Error> {
    let mut failure = FAILURE
        .lock()
        .map_err(|_| AdminError::Backend("live-export failure injection poisoned".into()))?;
    let Some((_, _, count)) = failure
        .iter_mut()
        .find(|(selected, step, count)| *selected == root && *step == phase && *count > 0)
    else {
        return Ok(false);
    };
    *count -= 1;
    Ok(true)
}

#[cfg(test)]
fn failure(root: RootId, phase: &str) -> Result<(), Error> {
    if armed(root, phase)? {
        return Err(AdminError::DiskFull(std::io::ErrorKind::StorageFull.into()).into());
    }
    Ok(())
}

#[cfg(test)]
fn renumbered(root: RootId, identity: fsutil::FileIdentity) -> Result<fsutil::FileIdentity, Error> {
    Ok(if armed(root, RENUMBERED_COPY)? {
        fsutil::FileIdentity {
            inode: !identity.inode,
            ..identity
        }
    } else {
        identity
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveResult {
    Pending,
    Complete,
    Conflict,
    Obsolete,
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
        result: LiveResult,
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

    fn cancel(&self, result: LiveResult) -> Self {
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
    tracked: Option<SyncRecord>,
    /// The staged copy's identity; installation replaces it with the one it confirmed.
    staged: Option<fsutil::FileIdentity>,
    purpose: Purpose,
}

/// Whether `file` still holds the tracked body. Size and mtime rule out an edit cheaply; the
/// hashes decide the rest.
fn matches_tracked(file: &File, tracked: &SyncRecord) -> Result<bool, Error> {
    let before = fsutil::stat_identity(file)?;
    let fp = &tracked.fingerprint;
    if before.size != fp.size || before.modified_ns != fp.mtime_ns {
        return Ok(false);
    }
    // Permission repair changes ctime, and Docker Desktop's shared folders can report a new
    // inode number for an unchanged file; stable hashes still prove the tracked body.
    Ok(
        content_matches(file, fp.size, fp.md5, fp.crc32c)?
            && before == fsutil::stat_identity(file)?,
    )
}

fn content_matches(file: &File, size: u64, md5: [u8; 16], crc32c: u32) -> Result<bool, Error> {
    Ok(verified_identity(file, size, md5, crc32c)?.is_some())
}

/// The identity of `file` when it holds exactly `size` bytes with these hashes and did not change
/// while they were read; `None` otherwise.
fn verified_identity(
    file: &File,
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
) -> Result<Option<fsutil::FileIdentity>, Error> {
    let before = fsutil::stat_identity(file)?;
    if before.size != size {
        return Ok(None);
    }
    let fsutil::HashOutcome::Complete(hashes) =
        fsutil::hash_exact(file, size, |_| Ok::<_, io::Error>(()))?
    else {
        return Ok(None);
    };
    let unchanged =
        before == fsutil::stat_identity(file)? && hashes.md5 == md5 && hashes.crc32c == crc32c;
    Ok(unchanged.then_some(before))
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

/// How a private copy compares with the identity it was staged under.
enum CopyMatch {
    /// It is the staged copy; the identity is the one to compare it with from now on.
    Staged(fsutil::FileIdentity),
    /// It is not, or it changed while it was checked.
    Changed {
        changes: fsutil::IdentityChanges,
        content: CopyContent,
    },
}

/// Whether a changed private copy's bytes were read.
#[derive(Clone, Copy)]
enum CopyContent {
    /// Fields other than the inode number changed, so the bytes were not read.
    NotChecked,
    /// Only the inode number differed, but the bytes, or the identity while they were read,
    /// did not match.
    Differs,
}

impl CopyContent {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotChecked => "not_checked",
            Self::Differs => "differs",
        }
    }
}

/// Compares the private copy `file` with the identity `staged` it was staged under; every check
/// of a private copy against that identity uses this one rule. Every field must match, except
/// that the inode number alone may differ ([`fsutil::IdentityChanges::is_inode_only`]): the same
/// handle then proves the staged bytes by size and hashes, and the identity observed while they
/// were read must still differ in nothing but the inode number.
fn compare_copy(
    file: &File,
    staged: fsutil::FileIdentity,
    item: &Pending,
) -> Result<CopyMatch, Error> {
    let current = fsutil::stat_identity(file)?;
    let changes = staged.changed_fields(&current);
    if changes.is_empty() {
        return Ok(CopyMatch::Staged(current));
    }
    if !changes.is_inode_only() {
        return Ok(CopyMatch::Changed {
            changes,
            content: CopyContent::NotChecked,
        });
    }
    let confirmed =
        verified_identity(file, item.size, item.md5, item.crc32c)?.filter(|confirmed| {
            let changes = staged.changed_fields(confirmed);
            changes.is_empty() || changes.is_inode_only()
        });
    Ok(match confirmed {
        Some(confirmed) => CopyMatch::Staged(confirmed),
        None => CopyMatch::Changed {
            changes,
            content: CopyContent::Differs,
        },
    })
}

/// Confirms that the private copy `file` is still the copy staged as `staged`
/// ([`compare_copy`]), and returns the identity to compare it with from now on. Anything else is
/// `Protected`, logged with the changed fields and whether the bytes were checked.
fn confirm_copy(
    file: &File,
    staged: fsutil::FileIdentity,
    item: &Pending,
    check: CopyCheck,
) -> Result<fsutil::FileIdentity, Error> {
    let (changes, content) = match compare_copy(file, staged, item)? {
        CopyMatch::Staged(identity) => return Ok(identity),
        CopyMatch::Changed { changes, content } => (changes, content),
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
) -> Result<bool, Error> {
    if let Some(device) = device {
        let identity = fsutil::stat_identity(file)?;
        // Rename changes ctime; the pre-exchange capture must still match every original observation.
        if identity.device != device
            || (stage == IdentityStage::BeforeExchange
                && identity.changed_ns != expected.fingerprint.ctime_ns)
        {
            return Ok(false);
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
fn observe_target(root: &File, item: &Pending) -> Result<bool, Error> {
    match target(root, &item.path)? {
        Some(file) => content_matches(&file, item.size, item.md5, item.crc32c),
        None => Ok(false),
    }
}

/// Installs the staged copy at the target path (or moves a removed file into the workspace)
/// unless an earlier attempt already did; `Ok(false)` reports an external change.
fn install_target(root: &File, workspace: &File, install: &mut Install) -> Result<bool, Error> {
    let selected = &install.item;
    let expected = install.tracked.as_ref();
    let staged = install.staged;
    let source_device = install.purpose.source_device();
    let current = target(root, &selected.path)?;
    let parents = selected.path.rsplit_once('/').ok_or_else(stale)?.0;
    let directory = crate::sync::tree::parent(
        root,
        std::path::Path::new(parents),
        crate::sync::tree::ParentOpen::Create,
    )?;
    let name = selected.path.rsplit('/').next().ok_or_else(stale)?;
    let retained = entry(workspace, selected.token.id)?;
    let already_installed = match staged {
        Some(staged) => current
            .as_ref()
            .map(fsutil::stat_identity)
            .transpose()?
            .is_some_and(|f| f.node() == staged.node()),
        None => retained.is_some(),
    };
    if !already_installed {
        if matches!(install.purpose, Purpose::RestoreAbsent) && current.is_some() {
            return Ok(false);
        }
        if let Some(staged) = staged {
            let copy = retained.as_ref().ok_or(Error::Protected)?;
            install.staged = Some(confirm_copy(copy, staged, selected, CopyCheck::Install)?);
        }
        let identity = match (&current, expected) {
            (None, _) => None,
            (Some(file), Some(expected))
                if matches_source(
                    file,
                    expected,
                    source_device,
                    IdentityStage::BeforeExchange,
                )? =>
            {
                Some(fsutil::stat_identity(file)?)
            }
            _ => return Ok(false),
        };
        let outcome = if selected.kind == Kind::Remove && current.is_none() {
            fsutil::ReplaceOutcome::Created
        } else if selected.kind == Kind::Publish {
            fsutil::durable_replace_between(
                workspace,
                selected.token.id.to_hex().as_ref(),
                &directory,
                name.as_ref(),
                identity.as_ref(),
            )?
        } else {
            fsutil::durable_replace_between(
                &directory,
                name.as_ref(),
                workspace,
                selected.token.id.to_hex().as_ref(),
                None,
            )?
        };
        if matches!(outcome, fsutil::ReplaceOutcome::Conflict { .. }) {
            return Err(Error::Protected);
        }
    }
    #[cfg(test)]
    failure(selected.token.root, "installed")?;
    if let Some(file) = entry(workspace, selected.token.id)? {
        let matches = match expected {
            Some(expected) => {
                matches_source(&file, expected, source_device, IdentityStage::Displaced)?
            }
            None => false,
        };
        if !matches {
            return Err(Error::Protected);
        }
        #[cfg(test)]
        failure(selected.token.root, "displaced-unlink")?;
        fsutil::unlink_file_at(workspace, selected.token.id.to_hex().as_ref())?;
    }
    #[cfg(test)]
    failure(selected.token.root, "displaced-parent-sync")?;
    directory.sync_all()?;
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(true)
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
            let path = match mapping::name_to_path(&bucket, &name) { PathMapping::File(p) => p.to_str().ok_or_else(stale)?.to_owned(), PathMapping::Unrepresentable(_) => return Err(stale()) };
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
    ) -> Result<LiveResult, Error> {
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
                    Ok(LiveResult::Pending)
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
                // `fsutil::clone_or_copy_into` says why.
                let partial = writable_entry(workspace, selected.token.id)?;
                if let Some(file) = &partial {
                    file.set_len(0)?;
                    #[cfg(test)]
                    faults.truncated(file)?;
                }
                #[cfg(test)]
                faults.hit(Step::SyncCopyWrite)?;
                match &partial {
                    Some(file) => fsutil::clone_or_copy_onto(&source, file)?,
                    None => fsutil::clone_or_copy_into(
                        &source,
                        workspace,
                        selected.token.id.to_hex().as_ref(),
                    )?,
                };
                let file = entry(workspace, selected.token.id)?.ok_or_else(stale)?;
                if !content_matches(&file, selected.size, selected.md5, selected.crc32c)? {
                    return Err(AdminError::Checksum.into());
                }
                #[cfg(test)]
                faults.hit(Step::SyncCopySync)?;
                file.sync_all()?;
                workspace.sync_all()?;
                #[cfg(test)]
                {
                    faults.synced(&file)?;
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
    async fn install_live(&self, operation: &mut LiveOperation) -> Result<LiveResult, Error> {
        if let Some(conflict) = operation.purpose.conflict() {
            conflict.verify_owned(&self.lease).await?;
        }
        let purpose = operation.purpose.clone();
        let mut item = operation.item.clone();
        let selected = item.clone();
        if item.phase == Phase::Prepared {
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
                    item = armed;
                    operation.item = item.clone();
                }
                Err(Error::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                    operation.progress = operation.progress.cancel(LiveResult::Obsolete);
                    self.cancel_live_copy(operation).await?;
                    return Ok(LiveResult::Obsolete);
                }
                Err(error) => return Err(error),
            }
        }
        let staged = operation.progress.staged();
        let install = Install {
            item: item.clone(),
            tracked: operation.tracked.clone(),
            staged,
            purpose: operation.purpose.clone(),
        };
        let observing = matches!(operation.body, Body::Observe { .. });
        #[cfg(test)]
        let faults = self.fault_paths();
        // The job hands back the staged identity installation confirmed even when it fails
        // afterwards, so a retry, a cancellation and the displacement check compare against it.
        let attempt = self
            .fs(move |root, workspace| {
                let mut install = install;
                #[cfg(test)]
                failure(install.item.token.root, "install")?;
                let installed = if observing {
                    observe_target(root, &install.item)
                } else {
                    install_target(root, workspace, &mut install)
                };
                #[cfg(test)]
                let installed = installed.and_then(|installed| {
                    if installed && !observing {
                        faults.installed(&install.item);
                        faults.hit(Step::SyncCopyInstalled)?;
                    }
                    Ok(installed)
                });
                Ok((install.staged, installed))
            })
            .await;
        let (staged, installed) = attempt.unwrap_or_else(|error| (staged, Err(error)));
        operation.progress = Progress::Staged(staged);
        match installed {
            Ok(false) => {
                operation.progress = operation.progress.cancel(LiveResult::Conflict);
                self.cancel_live_copy(operation).await?;
                return Ok(LiveResult::Conflict);
            }
            Ok(true) => operation.progress = Progress::Installed(staged),
            Err(Error::Protected) => {
                operation.item = self.inspect_inner(item.token.clone()).await?;
                return Err(Error::Protected);
            }
            Err(error) => {
                let selected = item.clone();
                let displaced = self.displaced_live(selected, staged).await?;
                if displaced {
                    let pending = self.inspect_inner(item.token.clone()).await?;
                    operation.item = pending.clone();
                    self.mark_entry_failure(pending.token).await?;
                    // Recovery owns displaced bytes; an automatic retry must not unlink them.
                    return Err(Error::Displaced(Box::new(error)));
                }
                return Err(error);
            }
        }
        Ok(LiveResult::Pending)
    }

    /// Determines whether a failed installation has already displaced the target.
    async fn displaced_live(
        &self,
        item: Pending,
        staged: Option<fsutil::FileIdentity>,
    ) -> Result<bool, Error> {
        self.fs(move |root, workspace| {
            if let Some(staged) = staged {
                // Another file in the private copy's entry means the namespace already changed.
                // The copy itself under another inode number is not one: a failure before
                // installation confirmed the copy leaves `staged` as it was recorded.
                if let Some(retained) = entry(workspace, item.token.id)?
                    && fsutil::stat_identity(&retained)?.node() != staged.node()
                    && matches!(
                        compare_copy(&retained, staged, &item)?,
                        CopyMatch::Changed { .. }
                    )
                {
                    return Ok(true);
                }
                let current = target(root, &item.path)?
                    .as_ref()
                    .map(fsutil::stat_identity)
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
    ) -> Result<LiveResult, Error> {
        if let Some(conflict) = operation.purpose.conflict() {
            conflict.verify_owned(&self.lease).await?;
        }
        let item = operation.item.clone();
        #[cfg(test)]
        failure(item.token.root, "durable")?;
        let selected = item.clone();
        let observed_ns = self.lease.observation_time_ns()?;
        let fp = match self
            .fs(move |root, workspace| fingerprint(root, workspace, &selected, observed_ns))
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
                operation.progress = operation.progress.cancel(LiveResult::Conflict);
                self.cancel_live_copy(operation).await?;
                return Ok(LiveResult::Conflict);
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
        Ok(LiveResult::Pending)
    }

    /// Verifies the durable fingerprint still holds, then tracks it and finishes the operation.
    async fn acknowledge_live(
        &self,
        operation: &mut LiveOperation,
        fp: Option<SyncFingerprint>,
    ) -> Result<LiveResult, Error> {
        if let Some(conflict) = operation.purpose.conflict() {
            conflict.verify_owned(&self.lease).await?;
        }
        let purpose = operation.purpose.clone();
        let item = operation.item.clone();
        let selected = item.clone();
        let expected = fp.clone();
        let verified = self
            .fs(move |root, workspace| {
                verify_fingerprint(root, workspace, &selected, expected.as_ref())
            })
            .await;
        match verified {
            Ok(()) => {}
            Err(Error::Protected) => {
                operation.item = self.inspect_inner(item.token.clone()).await?;
                return Err(Error::Protected);
            }
            Err(Error::Admin(AdminError::Precondition | AdminError::NotFound))
                if matches!(operation.body, Body::Observe { .. }) =>
            {
                operation.progress = operation.progress.cancel(LiveResult::Conflict);
                self.cancel_live_copy(operation).await?;
                return Ok(LiveResult::Conflict);
            }
            Err(error) => return Err(error),
        }
        // `fingerprint` reports a body exactly when one was published.
        let tracking = match (fp, &operation.body) {
            (Some(fp), Body::Copy { record, .. } | Body::Observe { record }) => {
                Some((fp, record.clone()))
            }
            (None, Body::Remove) => None,
            (Some(_), Body::Remove) | (None, Body::Copy { .. } | Body::Observe { .. }) => {
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
                if let Some((fp, record)) = tracking {
                    let update = SyncUpdate::new(binding,SyncPath::try_from(current.path.clone())?,fp)?;
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
            Ok(if valid { LiveResult::Complete } else { LiveResult::Obsolete })
        }).await?;
        #[cfg(test)]
        self.fault_paths().hit(Step::SyncCopyCommit)?;
        Ok(result)
    }

    async fn cancel_live_copy(&self, operation: &mut LiveOperation) -> Result<(), Error> {
        let item = operation.item.clone();
        let selected = item.clone();
        let staged = operation.progress.staged();
        let cancelled = self
            .fs(move |root, workspace| {
                if let Some(staged) = staged
                    && target(root, &selected.path)?
                        .as_ref()
                        .map(fsutil::stat_identity)
                        .transpose()?
                        .is_some_and(|identity| identity.node() == staged.node())
                {
                    return Err(Error::Protected);
                }
                if let Some(file) = entry(workspace, selected.token.id)? {
                    let (Kind::Publish, Some(staged)) = (selected.kind, staged) else {
                        return Err(Error::Protected);
                    };
                    confirm_copy(&file, staged, &selected, CopyCheck::Cancel)?;
                    fsutil::unlink_file_at(workspace, selected.token.id.to_hex().as_ref())?;
                    workspace.sync_all()?;
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
