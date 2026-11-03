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
    Settled(Change),
}
#[derive(Clone, Copy)]
pub(super) enum ControlState {
    Quiet,
    Changing,
}
impl DirectoryWatch {
    pub(super) fn control_state(&self) -> ControlState {
        if self.pending.is_some() {
            ControlState::Changing
        } else {
            ControlState::Quiet
        }
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
                    self.pending=None;
                    return Ok(Observed::Settled(change));
                }
            }
        }
    }
}

impl LiveControl<'_> {
    pub(super) async fn handle_source_changing(&mut self, request: Command) -> io::Result<()> {
        use crate::sync::control::{WaitReason, WaitResult};
        match request {
            Command::Wait {
                deadline,
                reply,
                identity,
            } => {
                let result = self
                    .root
                    .authorize_controls(self.config.seed_project.clone(), identity)
                    .await
                    .map_err(io::Error::other)
                    .map(|()| {
                        WaitResult::result(
                            if tokio::time::Instant::now() >= deadline {
                                WaitReason::Timeout
                            } else {
                                WaitReason::SourceChanging
                            },
                            None,
                            None,
                        )
                    });
                let _ = reply.send(result);
            }
            Command::Rescan { reply, identity } => {
                let result = self
                    .root
                    .authorize_controls(self.config.seed_project.clone(), identity)
                    .await
                    .map_err(io::Error::other)
                    .map(|()| Reconciliation {
                        source_changing: true,
                        deferred: 1,
                        ..Default::default()
                    });
                let _ = reply.send(result);
            }
            request => self.handle_authorized(request).await?,
        }
        Ok(())
    }
}
