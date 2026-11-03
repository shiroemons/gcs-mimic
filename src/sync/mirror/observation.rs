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
}
impl DirectoryWatch {
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
                    let quiet=if pending.quiet<=pending.limit {Quiet::Established} else {Quiet::Unverified};
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
    pub(super) fn ready(&mut self, watcher: &DirectoryWatch) -> Option<Command> {
        if watcher.pending.is_none() {
            self.0.pop_front()
        } else {
            None
        }
    }
    pub(super) fn admit(&mut self, request: Command, watcher: &DirectoryWatch) -> Option<Command> {
        if watcher.pending.is_some()
            && matches!(&request, Command::Wait { .. } | Command::Rescan { .. })
        {
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
