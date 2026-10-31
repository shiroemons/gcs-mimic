use super::{Catalog, unavailable};
use crate::{
    bucket::CatalogError,
    storage::sync_state::root::RootLease,
    sync::control::{Command, Registration},
};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};

impl Catalog {
    pub(crate) async fn wait_for_sync_shutdown(&self) {
        let shutdown = self.0.admin_shutdown.notified();
        tokio::pin!(shutdown);
        shutdown.as_mut().enable();
        if self.is_healthy() {
            shutdown.await
        }
    }

    pub(crate) fn register_sync_controls(
        &self,
        root: &Arc<RootLease>,
        sender: mpsc::Sender<Command>,
    ) -> Result<(), CatalogError> {
        let mut session = [0; 16];
        getrandom::fill(&mut session).map_err(|error| unavailable(std::io::Error::other(error)))?;
        let mut registration = self.0.sync_control.lock().map_err(unavailable)?;
        *registration = Some(Registration {
            session: crate::hashing::encode_lower(&session),
            root: Arc::downgrade(root),
            sender,
            admission: Arc::new(Semaphore::new(32)),
        });
        Ok(())
    }

    pub(crate) fn sync_controls(
        &self,
    ) -> Result<Option<(Registration, Arc<RootLease>)>, CatalogError> {
        let registration = self.0.sync_control.lock().map_err(unavailable)?;
        Ok(registration
            .as_ref()
            .and_then(|control| control.root.upgrade().map(|root| (control.clone(), root))))
    }

    pub(crate) fn clear_sync_controls(&self) -> Result<(), CatalogError> {
        let mut registration = self.0.sync_control.lock().map_err(unavailable)?;
        if let Some(control) = registration.take() {
            control.admission.close();
        }
        Ok(())
    }
}
