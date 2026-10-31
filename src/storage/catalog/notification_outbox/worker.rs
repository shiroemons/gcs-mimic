use super::{Catalog, unavailable, unix_micros};
use crate::{bucket::CatalogError, notification::Endpoint, task::TaskHealth};
use std::{
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

    pub(crate) fn start_notification_publisher(
        &self,
        endpoint: Endpoint,
    ) -> Result<(), CatalogError> {
        if self.0.notification_started.swap(true, Ordering::AcqRel) {
            return Err(unavailable("notification publisher already started"));
        }
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
    let _activity = catalog.activity()?;
    let Some(pending) = catalog.next_notification().await? else {
        health.pass_succeeded();
        return Ok(None);
    };
    let now = unix_micros(catalog.0.clock.now())?;
    if pending.due > now {
        return Ok(Some(Duration::from_micros(
            pending.due.saturating_sub(now) as u64
        )));
    }
    match endpoint.publish(&pending.topic, pending.message).await {
        Ok(()) => {
            catalog.acknowledge_notification(pending.id).await?;
            health.pass_succeeded();
        }
        Err(error) => {
            let delay = Duration::from_millis((100_u64 << pending.attempts.min(9)).min(30_000));
            let due = unix_micros(catalog.0.clock.now())?.saturating_add(delay.as_micros() as i64);
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
