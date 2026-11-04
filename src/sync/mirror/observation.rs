use super::*;
use crate::source_tree::watch::Change;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

pub(super) struct Pending {
    change: Change,
    quiet: tokio::time::Instant,
    limit: tokio::time::Instant,
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
        let settle = Duration::from_millis(super::super::SETTLE_MS);
        if let Some(pending) = &mut self.pending {
            if change == Change::Structure {
                pending.change = change;
            }
            pending.quiet = now + settle;
        } else {
            self.pending = Some(Pending {
                change,
                quiet: now + settle,
                limit: now + settle * 10,
            });
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
        let now = tokio::time::Instant::now();
        let settle = Duration::from_millis(super::super::SETTLE_MS);
        self.pending = Some(Pending {
            change: Change::Content,
            quiet: now + settle,
            limit: now + settle * 10,
        });
    }
    pub(super) async fn next_observation(&mut self) -> io::Result<Observed> {
        if self.pending.is_none() {
            let change = self.watch.changed().await?;
            self.record(change)?;
            return Ok(Observed::Pending);
        }
        loop {
            let pending = self
                .pending
                .as_ref()
                .ok_or_else(|| io::Error::other("missing pending observation"))?;
            let deadline = pending.quiet.min(pending.limit);
            tokio::select! {
                change=self.watch.changed()=>{
                    self.record(change?)?;
                    // A continuously readable native queue must still let the actor poll control requests.
                    tokio::task::yield_now().await;
                }
                ()=tokio::time::sleep_until(deadline)=>{
                    let pending=self.pending.take().ok_or_else(||io::Error::other("missing pending observation"))?;
                    let change=pending.change;
                    let quiet=if pending.quiet<=pending.limit {Quiet::Established} else {Quiet::Capped};
                    return Ok(Observed::Settled(change,quiet));
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
    pub(super) async fn reset(
        self,
        root: &RootLease,
        summary: &Reconciliation,
    ) -> Result<(), RootError> {
        let generation = self.generation?;
        self.watcher.poll_changes()?;
        if generation == Some(self.watcher.generation)
            && self.watcher.pending.is_none()
            && summary.baseline_complete
            && !summary.interrupted
            && summary.deferred == 0
            && summary.unrepresentable == 0
        {
            root.reset_empty_deletion_history().await?;
        }
        Ok(())
    }
    pub(super) async fn finish(self, root: &RootLease, summary: &Reconciliation) {
        #[cfg(test)]
        export_checkpoint("history-scanned").await;
        if let Err(error) = self.reset(root, summary).await {
            diagnostics::emit(
                LogLevel::Warn,
                "sync_history_reset_failed",
                serde_json::json!({"reason":crate::error::Report(&error).to_string()}),
            );
        }
    }
}

pub(super) async fn refresh(
    root: &RootLease,
    watcher: &mut DirectoryWatch,
    quiet: &mut Quiet,
    publication: &mut Option<crate::storage::sync_state::observation::Publication>,
    controls: &Controls,
) -> io::Result<()> {
    let changed = watcher.poll_changes()?;
    if watcher.pending.is_some() {
        if changed || *quiet == Quiet::Established {
            *quiet = Quiet::Unverified;
        }
        controls.changing();
        let started = publication.is_none();
        if started {
            *publication = Some(
                root.begin_observation_publication()
                    .await
                    .map_err(io::Error::other)?,
            );
        }
        if changed || started {
            root.invalidate_deletions()
                .await
                .map_err(io::Error::other)?;
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
                    let _ = reply.send(Err(io::ErrorKind::Interrupted.into()));
                }
                Command::Approve { reply, .. } => {
                    let _ = reply.send(Err(io::ErrorKind::Interrupted.into()));
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
            _ => watcher.pending.is_none() || quiet == Quiet::Capped,
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
            _ => false,
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
                _ => None,
            })
            .min();
        let Some(deadline) = deadline else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(deadline).await;
    }
    pub(super) async fn expire(&mut self, root: &RootLease, config: &Config) -> io::Result<()> {
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
                        .map_err(io::Error::other)
                        .map(|()| {
                            crate::sync::control::WaitResult::result(
                                crate::sync::control::WaitReason::SourceChanging,
                                None,
                                None,
                            )
                        });
                    let _ = reply.send(result);
                }
                request => retained.push_back(request),
            }
        }
        self.0 = retained;
        Ok(())
    }
}
