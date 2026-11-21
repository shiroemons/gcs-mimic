//! One full reconciliation of the tree with the API.

use super::{
    SkippedExports, reconcile_live_object,
    retry::{DirectoryCreation, create_directory},
};
use crate::sync::mirror::BaselineGate;
use crate::{
    object::ObjectRecord,
    storage::{
        Catalog, ObjectListQuery,
        admin::{Admin, AdminError, BucketName, ObjectName},
        change_journal::JournalFence,
        sync_state::{
            SyncPath,
            root::RootLease,
            writeback::{Stop, Writeback},
        },
    },
    sync::{
        Pass, Reconciliation, SyncError,
        entries::{EntryClass, classify},
        interrupt::{Interrupt, Interrupted},
    },
};
use std::{io, path::PathBuf};

pub async fn reconcile(catalog: &Catalog, root: &RootLease) -> io::Result<Reconciliation> {
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    reconcile_until(catalog, root, &stopping, BaselineGate::Allowed)
        .await
        .map_err(io::Error::from)
}

/// Shared state of one full reconciliation pass.
struct Reconciler<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    interrupt: Interrupt<'a>,
    admin: Admin,
    writeback: Writeback,
    fence: JournalFence,
    /// The counts of this pass; `reconcile_with` completes and returns them.
    summary: Reconciliation,
    /// The live run's skipped exports, which this pass's exports update.
    skipped: &'a mut SkippedExports,
}

async fn reconcile_until(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &tokio::sync::watch::Receiver<bool>,
    baseline: BaselineGate,
) -> Result<Reconciliation, SyncError> {
    let writeback = root.default_writeback().await?;
    crate::sync::naming::activate(&writeback).await?;
    // A single reconciliation has no later pass to run a skipped export in.
    reconcile_with(
        catalog,
        root,
        Interrupt::new(stopping),
        baseline,
        &writeback,
        &mut SkippedExports::default(),
    )
    .await
}
pub(in crate::sync) async fn reconcile_with(
    catalog: &Catalog,
    root: &RootLease,
    interrupt: Interrupt<'_>,
    baseline: BaselineGate,
    writeback: &Writeback,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    catalog.enable_change_journal().await?;
    let fence = catalog.current_fence().await?;
    if writeback.status().await?.stop() != Stop::Ready {
        return Err(SyncError::WritebackUnfinished(Pass::Reconcile));
    }
    let mut reconciler = Reconciler {
        catalog,
        root,
        interrupt,
        admin: root.sync_admin(),
        writeback: writeback.clone(),
        fence,
        summary: Reconciliation::default(),
        skipped,
    };
    if reconciler.buckets().await? == Step::Stopped || interrupt.check() != Interrupted::No {
        reconciler.summary.interrupted = true;
        return Ok(reconciler.summary);
    }
    let mut summary = reconciler.summary;
    external_files(root, &reconciler.admin, interrupt, &mut summary).await?;
    let end = catalog.current_fence().await?;
    summary.interrupted = interrupt.check() != Interrupted::No;
    // An API change during the pass may not have reached the tree yet, which the pass itself
    // cannot see; everything else that holds the baseline back is the pass's own hold.
    if baseline == BaselineGate::Allowed
        && summary.gate().hold().completes_baseline()
        && end == fence
    {
        root.complete_baseline().await?;
        summary.baseline_complete = true;
    }
    Ok(summary)
}

impl Reconciler<'_> {
    fn stopped(&self) -> bool {
        self.interrupt.check() != Interrupted::No
    }

    async fn live_object(
        &mut self,
        bucket: &BucketName,
        name: &ObjectName,
    ) -> Result<(), SyncError> {
        let outcome = reconcile_live_object(
            self.catalog,
            self.root,
            &self.writeback,
            bucket,
            name,
            self.fence,
            &mut self.summary,
        )
        .await?;
        self.skipped.record((bucket.clone(), name.clone()), outcome);
        Ok(())
    }

    async fn buckets(&mut self) -> Result<Step, SyncError> {
        let mut after = None;
        loop {
            if self.stopped() {
                return Ok(Step::Stopped);
            }
            let page = self.admin.list_all_buckets(after.as_deref(), 256).await?;
            let next = page.items.last().map(|entry| entry.bucket.name.clone());
            for entry in page.items {
                let Ok(bucket) = BucketName::new(entry.bucket.name.clone()) else {
                    self.summary.count_unmapped_object();
                    continue;
                };
                let directory = PathBuf::from(bucket.as_str());
                if !self.root.remember_sync_bucket(&bucket).await? {
                    self.summary.count_deferred(&directory);
                    continue;
                }
                // What occupies the directory's place is a tree entry, which the scans count. A
                // directory gone with files the inventory takes to approval stays gone, and so do
                // the exports into it that `LiveObject::export` holds back.
                if !crate::sync::deletion::bucket_directory_held(self.root, &bucket).await? {
                    match create_directory(self.root, directory.clone()).await? {
                        DirectoryCreation::Created => {}
                        DirectoryCreation::Finding => self.summary.count_unmapped_object(),
                        // Each export into it would wait for it in turn: the bucket waits whole.
                        DirectoryCreation::Deferred => {
                            self.summary.count_deferred(&directory);
                            continue;
                        }
                    }
                }
                if self.objects(&bucket).await? == Step::Stopped
                    || self.tracked_deletions(&bucket).await? == Step::Stopped
                {
                    return Ok(Step::Stopped);
                }
            }
            if !page.has_more {
                return Ok(Step::Continue);
            }
            after = next;
        }
    }

    async fn objects(&mut self, bucket: &BucketName) -> Result<Step, SyncError> {
        let mut query = ObjectListQuery::default();
        loop {
            let objects = self.admin.list_objects(bucket, query.clone(), 256).await?;
            for record in objects.items {
                if self.object(bucket, &record).await? == Step::Stopped {
                    return Ok(Step::Stopped);
                }
            }
            match objects.next {
                Some(next) => query.cursor = Some(next),
                None => return Ok(Step::Continue),
            }
        }
    }

    async fn object(
        &mut self,
        bucket: &BucketName,
        record: &ObjectRecord,
    ) -> Result<Step, SyncError> {
        if self.stopped() {
            return Ok(Step::Stopped);
        }
        let name = ObjectName::new(record.name.clone())?;
        self.live_object(bucket, &name).await?;
        Ok(Step::Continue)
    }

    /// Reconciles tracked paths whose objects no longer exist, exporting their deletion.
    async fn tracked_deletions(&mut self, bucket: &BucketName) -> Result<Step, SyncError> {
        let binding = self.root.sync_binding(bucket).await?;
        let mut after_path: Option<String> = None;
        loop {
            let after = after_path.take().map(SyncPath::try_from).transpose()?;
            let tracked = self
                .catalog
                .sync_state()
                .page(
                    binding.clone(),
                    after,
                    crate::storage::sync_state::PageLimit::LARGEST,
                )
                .await?;
            if tracked.is_empty() {
                return Ok(Step::Continue);
            }
            for record in tracked {
                if self.stopped() {
                    return Ok(Step::Stopped);
                }
                after_path = Some(record.path);
                let name = ObjectName::new(record.name)?;
                match self.admin.get_object(bucket, &name, None).await {
                    Err(AdminError::NotFound) => self.live_object(bucket, &name).await?,
                    Ok(_) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
}

/// Counts into `counts` the files the working tree holds that no live object accounts for, and
/// the entries it holds that no object could.
pub(in crate::sync) async fn external_files(
    root: &RootLease,
    admin: &Admin,
    interrupt: Interrupt<'_>,
    counts: &mut Reconciliation,
) -> Result<(), SyncError> {
    let naming = root.naming()?;
    let mut scan = crate::sync::tree::TreeScan::new(root.tree_limits(), root.lookup()?);
    while !scan.is_empty() && interrupt.check() == Interrupted::No {
        let (next, page) = root.file_job(move |root| scan.page(root)).await?;
        scan = next;
        for entry in page {
            if interrupt.check() != Interrupted::No {
                return Ok(());
            }
            match classify(&entry, naming) {
                EntryClass::Unreadable(io::ErrorKind::ResourceBusy) => {
                    return Err(io::Error::from(io::ErrorKind::ResourceBusy).into());
                }
                EntryClass::Unreadable(kind) => counts.count_read_failure(&entry.path, kind),
                EntryClass::Bucket(_) | EntryClass::Directory => {}
                EntryClass::Skipped(skip) => counts.count_skipped(&entry.path, skip),
                EntryClass::Unrepresentable(problem) => {
                    counts.count_unrepresentable_entry(&entry.path, problem);
                }
                EntryClass::File(file) => {
                    match admin.get_object(&file.bucket, &file.name, None).await {
                        Ok(_) => {}
                        Err(AdminError::NotFound) => counts.external_only += 1,
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
    }
    Ok(())
}

pub(in crate::sync) async fn hierarchy_collision(
    admin: &Admin,
    bucket: &BucketName,
    name: &ObjectName,
) -> Result<bool, AdminError> {
    let mut prefix = String::new();
    for part in name
        .as_str()
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent)
        .split('/')
        .filter(|part| !part.is_empty())
    {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        match admin
            .get_object(bucket, &ObjectName::new(prefix.clone())?, None)
            .await
        {
            Ok(_) => return Ok(true),
            Err(AdminError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let page = admin
        .list_objects(
            bucket,
            ObjectListQuery {
                prefix: format!("{}/", name.as_str()),
                ..Default::default()
            },
            1,
        )
        .await?;
    Ok(!page.items.is_empty())
}

/// Whether a reconciliation step finished or saw the stop signal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sync) enum Step {
    Continue,
    Stopped,
}
