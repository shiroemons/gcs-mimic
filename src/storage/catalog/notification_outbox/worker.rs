use super::{Catalog, unavailable, unix_micros};
use crate::{
    bucket::CatalogError,
    notification::{Endpoint, Topic},
    task::TaskHealth,
};
use std::{
    io,
    sync::{Arc, Weak, atomic::Ordering},
    time::Duration,
};
use tokio::sync::Notify;

impl Catalog {
    /// Configures the legacy event rule before seeding and starts at most one bounded publisher.
    pub async fn start_notifications(
        &self,
        config: &crate::config::Config,
    ) -> Result<(), CatalogError> {
        let runtime = config.notification_runtime().map_err(unavailable)?;
        self.configure_notifications(&runtime).await?;
        if let Some(endpoint) = runtime.endpoint {
            self.start_notification_publisher(endpoint)?;
        }
        Ok(())
    }

    /// Confirm with the configured local Pub/Sub emulator that `topic` exists. Without an
    /// emulator nothing can be confirmed, so the topic is accepted unchecked.
    pub(crate) async fn check_notification_topic(
        &self,
        topic: &Topic,
    ) -> Result<(), crate::notification::TopicCheckError> {
        match self.0.pubsub_endpoint.get() {
            Some(endpoint) => endpoint.topic_exists(topic).await,
            None => Ok(()),
        }
    }

    pub(crate) fn start_notification_publisher(
        &self,
        endpoint: Endpoint,
    ) -> Result<(), CatalogError> {
        if self.0.notification_started.swap(true, Ordering::AcqRel) {
            return Err(unavailable("notification publisher already started"));
        }
        // The swap above makes this the only writer, so the set cannot lose a race.
        let _ = self.0.pubsub_endpoint.set(endpoint.clone());
        let health = TaskHealth::default();
        self.watch_background_task(health.clone());
        let weak = Arc::downgrade(&self.0);
        let wake = self.0.notification_wake.clone();
        crate::task::spawn_restarting("notification-publisher", health.clone(), move || {
            let inner = weak.upgrade()?;
            (!inner.admission.is_closed())
                .then(|| run(weak.clone(), wake.clone(), endpoint.clone(), health.clone()))
        });
        Ok(())
    }
}

async fn run(
    weak: Weak<super::super::Inner>,
    wake: Arc<Notify>,
    endpoint: Endpoint,
    health: TaskHealth,
) {
    loop {
        let changed = wake.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let Some(inner) = weak.upgrade() else { return };
        if inner.admission.is_closed() {
            return;
        }
        let catalog = Catalog(inner);
        let delay = match attempt(&catalog, &endpoint, &health).await {
            Ok(delay) => delay,
            Err(error) => {
                if catalog.0.admission.is_closed() {
                    return;
                }
                health.pass_failed();
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "notification_catalog_retry",
                    serde_json::json!({"message": error.to_string()}),
                );
                Some(Duration::from_secs(1))
            }
        };
        drop(catalog);
        match delay {
            Some(delay) => {
                tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = changed => {} }
            }
            None => changed.await,
        }
    }
}

async fn attempt(
    catalog: &Catalog,
    endpoint: &Endpoint,
    health: &TaskHealth,
) -> Result<Option<Duration>, CatalogError> {
    let _attempt = catalog
        .0
        .notification_attempts
        .clone()
        .acquire_owned()
        .await
        .map_err(unavailable)?;
    if catalog.0.admission.is_closed() {
        return Ok(None);
    }
    // A reset cannot succeed between selecting a queued snapshot and finishing its network attempt.
    // Waiting out an exclusive job is not a failed pass, and close wakes this wait.
    let _activity = catalog.await_activity(super::super::Work::New).await?;
    let Some(pending) = catalog.next_notification().await? else {
        health.pass_succeeded();
        return Ok(None);
    };
    let now = unix_micros(catalog.0.clock.now())?;
    if pending.due > now {
        return Ok(Some(Duration::from_micros(
            u64::try_from(pending.due.saturating_sub(now)).map_err(unavailable)?,
        )));
    }
    let delivered = match queued_topic(&pending.topic) {
        Ok(topic) => endpoint.publish(&topic, pending.message).await,
        Err(error) => Err(error),
    };
    match delivered {
        Ok(()) => {
            catalog.acknowledge_notification(pending.id).await?;
            health.pass_succeeded();
        }
        Err(error) => {
            let delay = Duration::from_millis((100_u64 << pending.attempts.min(9)).min(30_000));
            let due = unix_micros(catalog.0.clock.now())?
                .saturating_add(i64::try_from(delay.as_micros()).map_err(unavailable)?);
            catalog.retry_notification(pending.id, due).await?;
            health.pass_failed();
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "notification_publish_retry",
                serde_json::json!({"message":error.to_string(),"outboxId":pending.id,"retryAfterMs":delay.as_millis()}),
            );
        }
    }
    Ok(Some(Duration::ZERO))
}

/// The topic a queued row names. Rows hold only topics this server queued, so the check is a
/// read boundary: a damaged row is retried like a failed delivery and never blocks the rows
/// queued after it.
fn queued_topic(stored: &str) -> io::Result<Topic> {
    Topic::from_canonical(stored)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid Pub/Sub topic"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clock::SystemClock,
        storage::admin::LocalAdmin,
        test_support::{TestDir, joined},
        time_limit::TimeLimit,
    };

    /// The outbox stores only full topic names, so a row with a relative one is damaged, and
    /// the retry it causes names the topic rather than the emulator endpoint.
    #[test]
    fn a_damaged_queued_topic_is_reported_as_an_invalid_topic() {
        for stored in ["projects/p/topics/topic", "not a topic"] {
            let error = queued_topic(stored).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{stored}");
            assert_eq!(error.to_string(), "invalid Pub/Sub topic", "{stored}");
        }
        assert!(queued_topic("//pubsub.googleapis.com/projects/p/topics/topic").is_ok());
    }

    /// Enough failed passes turn readiness off, so a reset must not count as one.
    #[tokio::test]
    async fn an_attempt_waits_out_an_exclusive_job_instead_of_failing_its_pass() {
        let dir = TestDir::new();
        let catalog = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Enabled)
            .await
            .unwrap();
        let exclusive = catalog
            .wait_exclusive(TimeLimit::from_millis(60_000).deadline())
            .await
            .unwrap();
        let worker = catalog.clone();
        let health = TaskHealth::default();
        let attempted = tokio::spawn(async move {
            let endpoint = Endpoint::parse("127.0.0.1:1").unwrap();
            attempt(&worker, &endpoint, &health).await.map(drop)
        });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(!attempted.is_finished());
        drop(exclusive);
        joined(attempted).await.unwrap();
        catalog.close().await.unwrap();
    }
}
