use super::*;
use crate::source_tree::watch::Change;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct Pending {
    change: Change,
    quiet: tokio::time::Instant,
    limit: tokio::time::Instant,
}
impl Pending {
    /// A change first seen at `now`: it settles after one quiet interval, and at the latest
    /// after ten.
    fn new(change: Change, now: tokio::time::Instant) -> Self {
        let settle = Duration::from_millis(super::super::SETTLE_MS);
        Self {
            change,
            quiet: now + settle,
            limit: now + settle * 10,
        }
    }

    /// How the change settled once its deadline passed.
    fn settled(self) -> Observed {
        let quiet = if self.quiet <= self.limit {
            Quiet::Established
        } else {
            Quiet::Capped
        };
        Observed::Settled(self.change, quiet)
    }
}
pub(super) enum Observed {
    Pending,
    Settled(Change, Quiet),
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Quiet {
    Established,
    Unverified,
    Capped,
}
impl DirectoryWatch {
    fn record(&mut self, change: Change) -> io::Result<()> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("native observation generation exhausted"))?;
        let now = tokio::time::Instant::now();
        if let Some(pending) = &mut self.pending {
            if change == Change::Structure {
                pending.change = change;
            }
            pending.quiet = now + Duration::from_millis(super::super::SETTLE_MS);
        } else {
            self.pending = Some(Pending::new(change, now));
        }
        Ok(())
    }
    pub(super) fn poll_changes(&mut self) -> io::Result<bool> {
        if let Some(change) = self.watch.try_changed()? {
            self.record(change)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub(super) fn history_scan(&mut self, quiet: Quiet) -> HistoryScan<'_> {
        let generation = self.poll_changes().map(|_| {
            if quiet == Quiet::Established && self.pending.is_none() {
                Some(self.generation)
            } else {
                None
            }
        });
        HistoryScan {
            watcher: self,
            generation,
        }
    }
    pub(super) fn continue_quiet(&mut self) {
        self.pending = Some(Pending::new(Change::Content, tokio::time::Instant::now()));
    }
    pub(super) async fn next_observation(&mut self) -> io::Result<Observed> {
        loop {
            let Some(pending) = self.pending else {
                let change = self.watch.changed().await?;
                self.record(change)?;
                return Ok(Observed::Pending);
            };
            tokio::select! {
                change=self.watch.changed()=>{
                    self.record(change?)?;
                    // A continuously readable native queue must still let the actor poll control requests.
                    tokio::task::yield_now().await;
                }
                ()=tokio::time::sleep_until(pending.quiet.min(pending.limit))=>{
                    self.pending=None;
                    return Ok(pending.settled());
                }
            }
        }
    }
}

pub(super) struct HistoryScan<'a> {
    watcher: &'a mut DirectoryWatch,
    generation: io::Result<Option<u64>>,
}
impl HistoryScan<'_> {
    pub(super) async fn reset(self, root: &RootLease, pass: PassGate) -> Result<(), RootError> {
        let generation = self.generation?;
        self.watcher.poll_changes()?;
        if generation == Some(self.watcher.generation)
            && self.watcher.pending.is_none()
            && pass.infers_deletions()
        {
            root.reset_empty_deletion_history().await?;
        }
        Ok(())
    }
    pub(super) async fn finish(self, root: &RootLease, pass: PassGate) {
        #[cfg(test)]
        export_checkpoint("history-scanned").await;
        if let Err(error) = self.reset(root, pass).await {
            diagnostics::emit(
                LogLevel::Warn,
                "sync_history_reset_failed",
                serde_json::json!({"reason":crate::error::Report(&error).to_string()}),
            );
        }
    }
}

/// How the observer holds native changes that it has not ingested yet: whether a quiet interval
/// followed them, and the publication that keeps readers on the last complete deletion set.
pub(super) struct Unsettled {
    pub(super) quiet: Quiet,
    publication: Option<crate::storage::sync_state::observation::Publication>,
}

/// Whether `Unsettled::publish` started the publication or found one held.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Publishing {
    Started,
    Held,
}

impl Unsettled {
    pub(super) fn new() -> Self {
        Self {
            quiet: Quiet::Unverified,
            publication: None,
        }
    }

    /// Starts the publication unless one is held.
    async fn publish(&mut self, root: &RootLease) -> Result<Publishing, SyncError> {
        if self.publication.is_some() {
            return Ok(Publishing::Held);
        }
        self.publication = Some(root.begin_observation_publication().await?);
        Ok(Publishing::Started)
    }

    /// Holds the publication and invalidates deletion candidates the change may have made stale.
    pub(super) async fn unsettle(&mut self, root: &RootLease) -> Result<(), SyncError> {
        self.publish(root).await?;
        root.invalidate_deletions().await?;
        Ok(())
    }

    /// Releases the publication once the change is ingested.
    pub(super) fn settle(&mut self) {
        self.publication = None;
    }
}

pub(super) async fn refresh(
    root: &RootLease,
    watcher: &mut DirectoryWatch,
    unsettled: &mut Unsettled,
    controls: &Controls,
) -> Result<(), SyncError> {
    let changed = watcher.poll_changes()?;
    if watcher.pending.is_some() {
        if changed || unsettled.quiet == Quiet::Established {
            unsettled.quiet = Quiet::Unverified;
        }
        controls.changing();
        let started = unsettled.publish(root).await? == Publishing::Started;
        if changed || started {
            root.invalidate_deletions().await?;
            #[cfg(test)]
            if started {
                export_checkpoint("observation-invalidated").await;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Controls(std::collections::VecDeque<Command>);
impl Controls {
    pub(super) fn changing(&self) {
        for request in &self.0 {
            if let Command::Wait { settling, .. } = request {
                settling.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
    pub(super) fn running(
        &mut self,
        request: Command,
        stopping: &tokio::sync::watch::Receiver<bool>,
    ) -> Option<Command> {
        if *stopping.borrow() {
            self.0.push_front(request);
            None
        } else {
            Some(request)
        }
    }
    pub(super) fn stop(&mut self) {
        for request in self.0.drain(..) {
            match request {
                Command::Wait { reply, .. } => {
                    let _ = reply.send(Ok(crate::sync::control::WaitResult::result(
                        crate::sync::control::WaitReason::Stopped,
                        None,
                        None,
                    )));
                }
                Command::Rescan { reply, .. } => {
                    let _ = reply.send(Err(ControlError::Shutdown));
                }
                Command::Approve { reply, .. } => {
                    let _ = reply.send(Err(ControlError::Shutdown));
                }
                Command::Resolve { reply, .. } => {
                    let _ = reply.send(Err(AdminError::Closed.into()));
                }
            }
        }
    }
    pub(super) fn has_waiters(&self) -> bool {
        !self.0.is_empty()
    }
    pub(super) async fn receive(
        &mut self,
        requests: &mut tokio::sync::mpsc::Receiver<Command>,
    ) -> Option<Command> {
        if let Some(request) = self.0.pop_front() {
            Some(request)
        } else {
            requests.recv().await
        }
    }
    pub(super) fn settled(&self) {
        for request in &self.0 {
            if let Command::Wait { settling, .. } = request {
                settling.store(false, std::sync::atomic::Ordering::Release);
            }
        }
    }
    pub(super) fn available(&self) -> bool {
        self.0.len() < 8
    }
    pub(super) fn ready(&mut self, watcher: &DirectoryWatch, quiet: Quiet) -> Option<Command> {
        let index = self.0.iter().position(|request| match request {
            Command::Wait { .. } => watcher.pending.is_none() && quiet == Quiet::Established,
            Command::Rescan { .. } | Command::Approve { .. } | Command::Resolve { .. } => {
                watcher.pending.is_none() || quiet == Quiet::Capped
            }
        })?;
        self.0.remove(index)
    }
    pub(super) fn admit(
        &mut self,
        request: Command,
        watcher: &DirectoryWatch,
        quiet: Quiet,
    ) -> Option<Command> {
        let waits = match &request {
            Command::Wait { .. } => true,
            Command::Rescan { .. } => quiet != Quiet::Capped,
            Command::Approve { .. } | Command::Resolve { .. } => false,
        };
        if watcher.pending.is_some() && waits {
            if let Command::Wait { settling, .. } = &request {
                settling.store(true, std::sync::atomic::Ordering::Release);
            }
            self.0.push_back(request);
            None
        } else {
            Some(request)
        }
    }
    pub(super) async fn expired(&self) {
        let deadline = self
            .0
            .iter()
            .filter_map(|request| match request {
                Command::Wait { deadline, .. } => Some(*deadline),
                Command::Rescan { .. } | Command::Approve { .. } | Command::Resolve { .. } => None,
            })
            .min();
        let Some(deadline) = deadline else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(deadline).await;
    }
    pub(super) async fn expire(
        &mut self,
        root: &RootLease,
        config: &Config,
    ) -> Result<(), SyncError> {
        let mut retained = std::collections::VecDeque::new();
        while let Some(request) = self.0.pop_front() {
            match request {
                Command::Wait {
                    deadline,
                    reply,
                    identity,
                    ..
                } if deadline <= tokio::time::Instant::now() => {
                    let result = root
                        .authorize_controls(config.seed_project.clone(), identity)
                        .await
                        .map_err(ControlError::from)
                        .map(|()| {
                            crate::sync::control::WaitResult::result(
                                crate::sync::control::WaitReason::SourceChanging,
                                None,
                                None,
                            )
                        });
                    let _ = reply.send(result);
                }
                request @ (Command::Wait { .. }
                | Command::Rescan { .. }
                | Command::Approve { .. }
                | Command::Resolve { .. }) => retained.push_back(request),
            }
        }
        self.0 = retained;
        Ok(())
    }
}
