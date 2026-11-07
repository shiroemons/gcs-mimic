//! How other work stays apart from an exclusive job such as a reset or a seed commit.
//!
//! - A SQL job holds the execution permit, and an exclusive job owns that permit from before it
//!   takes the gate until after it lets go. The two never overlap, so a caller queued for SQL
//!   simply waits its turn and needs no lease. Its queueing cannot keep the gate shut either.
//! - Work that spans several SQL jobs (an upload body, a session writer, a source pin) holds a
//!   read lease. An exclusive job starts only once every lease is gone.
//! - An exclusive job reserves the gate for as long as it waits. New work lines up behind the
//!   reservation, so a steady stream of uploads cannot keep leases out forever, while work that
//!   already holds a lease keeps going and finishes the drain the job is waiting for.
//!
//! Nothing here times out. Request handlers bound a wait with their own deadlines, dropping a
//! wait gives up its place at no cost, and closing the catalog wakes every waiter.

use super::{Catalog, CatalogError, admin::AdminError, unavailable};
use crate::{
    object::ObjectError,
    storage::failure::{BackendFailure, object_unavailable},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{
        Notify, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, OwnedSemaphorePermit, RwLock,
        TryLockError,
    },
    time::Instant,
};

pub(super) type Activity = OwnedRwLockReadGuard<()>;

/// What a caller waiting for a lease is about to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Work {
    /// Starts something while holding no lease, so it lines up behind a reservation: the job
    /// holding the reservation depends on nothing this caller could finish.
    New,
    /// Continues something already started, possibly under a lease of its own. It waits only
    /// while an exclusive job actually runs, which cannot happen under a lease, so it never waits
    /// for itself. Behind a reservation it could: the job may be draining this very lease.
    UnderWay,
}

/// The one way waiting for a lease fails.
#[derive(Clone, Copy, Debug)]
pub(super) struct ActivityClosed;

impl From<ActivityClosed> for CatalogError {
    fn from(_: ActivityClosed) -> Self {
        unavailable(BackendFailure::CatalogClosed)
    }
}

impl From<ActivityClosed> for ObjectError {
    fn from(_: ActivityClosed) -> Self {
        object_unavailable(BackendFailure::CatalogClosed)
    }
}

impl From<ActivityClosed> for AdminError {
    fn from(_: ActivityClosed) -> Self {
        Self::Closed
    }
}

#[derive(Default)]
pub(super) struct ActivityGate {
    lock: Arc<RwLock<()>>,
    reservations: Arc<Reservations>,
}

#[derive(Default)]
struct Reservations {
    /// Exclusive jobs that have started waiting and have not finished. A waiter registers with
    /// `released` before reading this and a job lowers this before notifying; sequential
    /// consistency keeps those orders across threads, so no waiter both misses the new count and
    /// misses the wakeup.
    count: AtomicUsize,
    /// Wakes the callers waiting for a lease when a reservation ends or the catalog closes.
    released: Notify,
}

impl ActivityGate {
    fn reserve(&self) -> Reservation {
        self.reservations.count.fetch_add(1, Ordering::SeqCst);
        Reservation(self.reservations.clone())
    }

    /// True from when an exclusive job starts waiting until it finishes.
    pub(super) fn is_reserved(&self) -> bool {
        self.reservations.count.load(Ordering::SeqCst) > 0
    }

    fn try_read(&self) -> Result<Activity, TryLockError> {
        self.lock.clone().try_read_owned()
    }

    /// Wakes every caller waiting for a lease, so that it sees the catalog closing.
    pub(super) fn wake_waiters(&self) {
        self.reservations.released.notify_waiters();
    }

    /// Resolves once every lease taken so far is gone.
    #[cfg(test)]
    pub(super) async fn drained(&self) {
        drop(self.lock.clone().write_owned().await);
    }
}

/// An exclusive job's place in line. It ends on every path out of the job: success, an expired
/// wait, a closed catalog and a dropped future.
struct Reservation(Arc<Reservations>);

impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.count.fetch_sub(1, Ordering::SeqCst);
        // Even with another reservation left: work under way waits only while a job runs, so the
        // end of this one may be all it needs.
        self.0.released.notify_waiters();
    }
}

/// Sole ownership of the catalog.
///
/// The fields drop in the order they release. A waiter woken by the reservation must find the
/// gate open, and a caller granted the execution permit must find no exclusive job left, which
/// is what lets SQL jobs run without a lease.
pub(super) struct ExclusiveGuard {
    _gate: OwnedRwLockWriteGuard<()>,
    _reservation: Reservation,
    _execution: OwnedSemaphorePermit,
}

impl Catalog {
    /// Takes a lease now or fails. This form suits work that can skip a turn, such as
    /// maintenance, and work that already holds a lease, where it cannot fail on an exclusive job.
    pub(super) fn activity(&self) -> Result<Activity, CatalogError> {
        self.0.activity.try_read().map_err(unavailable)
    }

    /// A lease for a read about to run, or `None` if an exclusive job runs or waits to run. The
    /// read then takes the serialized path, which lines up behind the job.
    pub(super) fn read_lease(&self) -> Option<Activity> {
        let gate = &self.0.activity;
        if gate.is_reserved() {
            return None;
        }
        gate.try_read().ok()
    }

    /// Waits for a lease. Only a closed catalog fails this, at once for new work; work under way
    /// may still finish while the catalog closes, so it fails only if it would have to wait.
    pub(super) async fn await_activity(&self, work: Work) -> Result<Activity, ActivityClosed> {
        let gate = &self.0.activity;
        loop {
            let released = gate.reservations.released.notified();
            tokio::pin!(released);
            // Registered before the checks, so a release or a close right after them still wakes
            // this waiter.
            released.as_mut().enable();
            let closed = self.0.admission.is_closed();
            let lease = match work {
                Work::New if closed || gate.is_reserved() => None,
                Work::New | Work::UnderWay => gate.try_read().ok(),
            };
            match lease {
                Some(lease) => return Ok(lease),
                None if closed => return Err(ActivityClosed),
                None => released.await,
            }
        }
    }

    /// As [`Self::await_activity`], for administrative work with its own deadline.
    pub(super) async fn admin_activity(
        &self,
        work: Work,
        deadline: Instant,
    ) -> Result<Activity, AdminError> {
        tokio::time::timeout_at(deadline, self.await_activity(work))
            .await
            .map_err(|_| AdminError::Busy {
                retry_after: Duration::from_secs(1),
            })?
            .map_err(AdminError::from)
    }

    /// Waits until in-flight catalog work drains, then owns SQL execution and the exclusive
    /// activity gate together; expiry returns `Busy` before anything is mutated.
    pub(super) async fn wait_exclusive(
        &self,
        deadline: Instant,
    ) -> Result<ExclusiveGuard, AdminError> {
        let reservation = self.0.activity.reserve();
        loop {
            let execution = self.wait_admin(self.0.execution.clone(), deadline).await?;
            // Queuing a writer would reject the nested read leases needed by draining uploads.
            if let Ok(gate) = self.0.activity.lock.clone().try_write_owned() {
                return Ok(ExclusiveGuard {
                    _gate: gate,
                    _reservation: reservation,
                    _execution: execution,
                });
            }
            // Draining work may need the execution slot to finish and release its lease.
            drop(execution);
            tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_millis(10)))
                .await;
        }
    }
}

#[cfg(test)]
mod tests;
