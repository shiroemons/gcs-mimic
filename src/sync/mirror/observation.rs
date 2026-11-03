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
        let settle = Duration::from_millis(super::super::SETTLE_MS);
        if self.pending.is_none() {
            let change = self.watch.changed().await?;
            let now = tokio::time::Instant::now();
            self.pending = Some(Pending {
                change,
                quiet: now + settle,
                limit: now + settle * 10,
            });
            return Ok(Observed::Pending);
        }
        loop {
            let pending = self
                .pending
                .as_mut()
                .ok_or_else(|| io::Error::other("missing pending observation"))?;
            tokio::select! {
                change=self.watch.changed()=>{
                    if change?==Change::Structure {pending.change=Change::Structure;}
                    pending.quiet=tokio::time::Instant::now()+settle;
                    // A continuously readable native queue must still let the actor poll control requests.
                    tokio::task::yield_now().await;
                }
                ()=tokio::time::sleep_until(pending.quiet.min(pending.limit))=>{
                    let change=pending.change;
                    let quiet=if pending.quiet<=pending.limit {Quiet::Established} else {Quiet::Capped};
                    self.pending=None;
                    return Ok(Observed::Settled(change,quiet));
                }
            }
        }
    }
}

pub(super) async fn reset_history(
    root: &RootLease,
    quiet: Quiet,
    summary: &Reconciliation,
) -> io::Result<()> {
    if quiet == Quiet::Established
        && summary.baseline_complete
        && !summary.interrupted
        && summary.deferred == 0
        && summary.unrepresentable == 0
    {
        root.reset_empty_deletion_history()
            .await
            .map_err(io::Error::other)?;
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Controls(std::collections::VecDeque<Command>);
impl Controls {
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
                _ => {}
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
