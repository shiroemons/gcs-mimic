//! Answers to repeated reads, held in memory until the next catalog write.
//!
//! A read on the pool costs two thread wakeups and a SQL snapshot, however fast each one is. A hit
//! costs neither, and it runs on the async worker because nothing in it can block. Object bytes
//! stay out of this cache: the page cache already holds them, so an entry keeps the file open and
//! [`crate::fsutil::read_cached_at`] reads them from there, at no memory cost of its own.
//!
//! No hit can outlive a write. Every write runs through `Inner::with_connection`, one process owns
//! the catalog, and a job that changed a row clears the cache before its caller can learn the
//! outcome, so a read that starts after a write finished misses and sees it. A read racing a write
//! may answer from either side, as a read on the pool may. A pooled read notes the [`Revision`]
//! before its snapshot, and its answer is dropped if a write finished since.
//!
//! Only answers that depend on nothing but committed state are shared: live objects read with
//! permissive access. Strict-mode authorization runs on every request, and objects under a
//! customer-supplied key never enter, so a hit cannot skip checking the key.
//!
//! Full means no more admissions until the next write, not eviction. A load test or a backup
//! sweeps its objects in a cycle, which keeps a recency policy missing forever, while a cache that
//! stops admitting serves the share that fits. The cost of a stale hot set is only the pooled read.

use super::Catalog;
use crate::{
    bucket::BucketAccess,
    object::{ObjectRecord, ObjectTarget},
};
use std::{
    collections::HashMap,
    fs::File,
    sync::{
        Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicU64, Ordering},
    },
};

/// What the records may take, since custom metadata makes one record much larger than another.
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// Four times the thousand objects per bucket this emulator is built for.
const MAX_OBJECTS: usize = 4096;
/// A hot object needs its descriptor, while one more open file costs little until a limit is near.
const MAX_OPEN_FILES: usize = 1024;
/// Caps the answers a client can create by naming buckets that do not exist.
const MAX_BUCKETS: usize = 1024;

pub(super) struct CachedObject {
    pub(super) record: ObjectRecord,
    /// Absent until a download stores it, since a metadata read needs neither part.
    pub(super) download: Option<CachedDownload>,
}

/// What a download needs beyond the record. The file's bytes cannot change: a write that replaces
/// the object clears the cache, and a reader still holding the file keeps its bytes. Only HTTP
/// downloads open through the cache, and they read by offset without taking the file over.
pub(super) struct CachedDownload {
    pub(super) file: Arc<File>,
    pub(super) publicly_readable: bool,
}

impl CachedObject {
    fn charge(&self) -> usize {
        let ObjectRecord {
            bucket,
            name,
            content_type,
            content_encoding,
            content_disposition,
            content_language,
            cache_control,
            custom_time,
            metadata,
            ..
        } = &self.record;
        let optional = [
            content_encoding,
            content_disposition,
            content_language,
            cache_control,
            custom_time,
        ]
        .into_iter()
        .flatten()
        .map(String::len)
        .sum::<usize>();
        let metadata = metadata
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>();
        size_of::<Self>()
            + 2 * (bucket.len() + name.len())
            + content_type.len()
            + optional
            + metadata
    }
}

/// The cache's state as a read began. An answer read after this may be stored under it.
#[derive(Clone, Copy)]
pub(super) struct Revision(u64);

pub(super) struct ReadCache {
    revision: AtomicU64,
    max_open_files: usize,
    contents: RwLock<Contents>,
}

#[derive(Default)]
struct Contents {
    /// Nested so that a lookup borrows both names instead of building a key.
    objects: HashMap<String, HashMap<String, Arc<CachedObject>>>,
    object_count: usize,
    open_files: usize,
    charged: usize,
    bucket_cors: HashMap<String, bool>,
}

impl Contents {
    fn object(&self, bucket: &str, name: &str) -> Option<&Arc<CachedObject>> {
        self.objects.get(bucket)?.get(name)
    }

    fn insert_object(&mut self, mut object: CachedObject, max_open_files: usize) {
        let previous = self.object(&object.record.bucket, &object.record.name);
        // A record read without opening the file must not close the one a download kept.
        if previous.is_some_and(|previous| previous.download.is_some() || object.download.is_none())
        {
            return;
        }
        // The record is still worth holding when only its descriptor does not fit.
        if self.open_files >= max_open_files {
            object.download = None;
        }
        let previous_charge = previous.map_or(0, |previous| previous.charge());
        let charged = self.charged - previous_charge + object.charge();
        let adds_nothing = previous.is_some() && object.download.is_none();
        let is_full = previous.is_none() && self.object_count == MAX_OBJECTS;
        if adds_nothing || is_full || charged > MAX_RECORD_BYTES {
            return;
        }
        self.object_count += usize::from(previous.is_none());
        self.open_files += usize::from(object.download.is_some());
        self.charged = charged;
        self.objects
            .entry(object.record.bucket.clone())
            .or_default()
            .insert(object.record.name.clone(), Arc::new(object));
    }
}

impl ReadCache {
    pub(super) fn new(max_open_files: usize) -> Self {
        Self {
            revision: AtomicU64::new(0),
            max_open_files: max_open_files.min(MAX_OPEN_FILES),
            contents: RwLock::default(),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Contents> {
        self.contents.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Contents> {
        self.contents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn revision(&self) -> Revision {
        Revision(self.revision.load(Ordering::Acquire))
    }

    /// Ends every answer read so far. The revision moves first and the check in [`Self::store`]
    /// runs under the lock, so an insert either sees the new revision or is cleared here.
    pub(super) fn invalidate(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        // Bound first: dropping it closes every open file, which no hit should wait behind.
        let stale = std::mem::take(&mut *self.write());
        drop(stale);
    }

    fn store(&self, revision: Revision, insert: impl FnOnce(&mut Contents)) {
        let mut contents = self.write();
        if self.revision.load(Ordering::Acquire) == revision.0 {
            insert(&mut contents);
        }
    }

    pub(super) fn store_object(&self, revision: Revision, object: CachedObject) {
        if object.record.customer_key_sha256.is_none() {
            self.store(revision, |contents| {
                contents.insert_object(object, self.max_open_files);
            });
        }
    }

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "stores the answer the catalog just read"
    )]
    pub(super) fn store_bucket_cors(&self, revision: Revision, bucket: &str, has_cors: bool) {
        self.store(revision, |contents| {
            if contents.bucket_cors.len() < MAX_BUCKETS {
                contents.bucket_cors.insert(bucket.to_owned(), has_cors);
            }
        });
    }
}

/// Whether this read's answer depends on nothing but committed state.
pub(super) fn is_shareable(target: &ObjectTarget, access: &BucketAccess) -> bool {
    target.generation.is_none() && matches!(access, BucketAccess::Permissive)
}

impl Catalog {
    /// A closing catalog, an exclusive job and a writer that failed to roll back turn hits away just
    /// as they do pooled reads.
    fn cache_available(&self) -> bool {
        !self.0.admission.is_closed()
            && !self.0.activity.is_reserved()
            && !self.0.resources.is_poisoned()
    }

    /// A hit never waits for anything, so a caller reading in a loop would hold its worker and
    /// starve every task queued there. Charging the runtime's cooperative budget makes it yield
    /// like any other resource does.
    pub(super) async fn cached_object(&self, target: &ObjectTarget) -> Option<Arc<CachedObject>> {
        tokio::task::coop::consume_budget().await;
        if !self.cache_available() {
            return None;
        }
        self.0
            .read_cache
            .read()
            .object(&target.bucket, &target.name)
            .cloned()
    }

    pub(super) async fn cached_bucket_cors(&self, bucket: &str) -> Option<bool> {
        tokio::task::coop::consume_budget().await;
        if !self.cache_available() {
            return None;
        }
        self.0.read_cache.read().bucket_cors.get(bucket).copied()
    }
}

#[cfg(test)]
mod tests;
