//! The connection slots of a full server. A client that arrives while every slot is taken asks
//! the longest-idle connection of either protocol to give up its slot.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Notify, futures::OwnedNotified};

/// Hands a connection slot from the longest-idle connection of any protocol to a client waiting
/// at a full server, as nginx reuses idle connections. Closing an idle HTTP/1 connection races
/// with a pooled client reusing it, and a POST that loses that race fails, so only as many close
/// as clients need a slot. An idle gRPC connection answers with GOAWAY, which spares a racing
/// call but still costs its client a new connection.
#[derive(Default)]
pub(crate) struct SlotReclaim {
    /// True while a `SlotRequest` lives and no connection has taken it.
    wanted: AtomicBool,
    /// Idle connections wait here in the order they became idle; `notify_one` wakes the oldest.
    /// With nobody waiting it stores a wake for the next connection to become idle. Shared so
    /// that a response body, which owns everything it holds, can take a place in line.
    idle_line: Arc<Notify>,
}

/// Asks idle connections for a slot until it drops, when the client got one by any route.
#[must_use = "dropping the guard withdraws the connection slot request"]
pub(crate) struct SlotRequest<'a>(&'a SlotReclaim);

impl Drop for SlotRequest<'_> {
    fn drop(&mut self) {
        self.0.wanted.store(false, Ordering::Release);
    }
}

impl SlotReclaim {
    pub(crate) fn request(&self) -> SlotRequest<'_> {
        self.wanted.store(true, Ordering::Release);
        self.idle_line.notify_one();
        SlotRequest(self)
    }

    pub(crate) fn join_line(self: &Arc<Self>) -> IdleTurn {
        let mut turn = Box::pin(Arc::clone(&self.idle_line).notified_owned());
        // An unpolled `Notified` is not in line yet, and the line's order is the point.
        turn.as_mut().enable();
        IdleTurn {
            slots: Arc::clone(self),
            turn,
        }
    }

    /// A wake can outlive the request that sent it, when another connection ended first and
    /// gave the client a slot. Only the connection that takes the request closes.
    pub(crate) fn take_request(&self) -> bool {
        self.wanted.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn is_wanted(&self) -> bool {
        self.wanted.load(Ordering::Acquire)
    }
}

/// A place in the idle line, which a connection holds for as long as it stays idle.
#[must_use = "dropping the guard gives up the connection's place in the idle line"]
pub(crate) struct IdleTurn {
    slots: Arc<SlotReclaim>,
    turn: std::pin::Pin<Box<OwnedNotified>>,
}

impl IdleTurn {
    /// Resolves at the front of the idle line. The place in line outlives this future, so
    /// `select!` can drop it at any time.
    pub(crate) async fn reached(&mut self) {
        self.turn.as_mut().await;
    }
}

impl Drop for IdleTurn {
    /// Hands a waiting client's wake to the next connection in line on every way out of the idle
    /// phase. A `Notified` forwards a wake it received while queued, but not the stored one it
    /// took when enabled, so a connection leaving before its next poll would lose that one and
    /// the client would wait for some other connection to end.
    fn drop(&mut self) {
        if self.slots.is_wanted() {
            self.slots.idle_line.notify_one();
        }
    }
}

/// The place in the idle line that a request's end takes for its connection, until the
/// connection's watch moves it into the idle phase.
///
/// hyper drops a response body once its last bytes are buffered and before it flushes them, so a
/// connection takes its place before its client can see the response end, and connections whose
/// clients finished one after another stand in that order. Joining the line only when the watch
/// next ran let a busy runtime queue a connection that finished later ahead of an older one, so a
/// waiting client closed the younger connection while the older one kept its slot.
pub(super) struct IdlePlace {
    slots: Arc<SlotReclaim>,
    taken: std::sync::Mutex<Option<IdleTurn>>,
}

impl IdlePlace {
    pub(super) fn new(slots: Arc<SlotReclaim>) -> Self {
        Self {
            slots,
            taken: std::sync::Mutex::new(None),
        }
    }

    /// Takes a place at the back of the line, giving up one that an earlier request took.
    pub(super) fn join(&self) {
        let turn = self.slots.join_line();
        let earlier = self.lock().replace(turn);
        drop(earlier);
    }

    /// The place the last request's end took, or a new one at the back of the line.
    pub(super) fn take(&self) -> IdleTurn {
        let taken = self.lock().take();
        taken.unwrap_or_else(|| self.slots.join_line())
    }

    /// Gives up the place that a request's end took once the next request has started, so a busy
    /// connection holds no place and passes on any wake it received.
    pub(super) fn clear(&self) {
        let taken = self.lock().take();
        drop(taken);
    }

    /// Every use replaces the whole value, so a panic while it was held left nothing half done.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<IdleTurn>> {
        crate::sync::lock(&self.taken)
    }
}

#[cfg(test)]
mod tests;
