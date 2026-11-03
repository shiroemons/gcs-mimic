use std::time::Duration;
use tokio::{sync::watch, time::Instant};
#[derive(Clone, Copy)]
pub(super) struct Interrupt<'a> {
    pub(super) stopping: &'a watch::Receiver<bool>,
    pub(super) deadline: Option<Instant>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Interrupted {
    No,
    Stopped,
    Deadline,
}
impl<'a> Interrupt<'a> {
    pub(super) fn new(stopping: &'a watch::Receiver<bool>) -> Self {
        Self {
            stopping,
            deadline: None,
        }
    }
    pub(super) fn check(self) -> Interrupted {
        if *self.stopping.borrow() {
            Interrupted::Stopped
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Interrupted::Deadline
        } else {
            Interrupted::No
        }
    }
    pub(super) async fn sleep(self, duration: Duration) -> Interrupted {
        if self.check() != Interrupted::No {
            return self.check();
        }
        let mut stopping = self.stopping.clone();
        tokio::select! {
            biased;
            _=stopping.changed()=>Interrupted::Stopped,
            ()=async {if let Some(deadline)=self.deadline {tokio::time::sleep_until(deadline).await} else {std::future::pending().await}}=>Interrupted::Deadline,
            ()=tokio::time::sleep(duration)=>Interrupted::No,
        }
    }
}
