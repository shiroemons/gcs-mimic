//! Transfer owns the execution lane once; the conflict backend must not acquire it again.

use super::*;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

impl Writeback {
    pub(crate) async fn preserve_protected(&self) -> Result<Status, Error> {
        let this = self.clone();
        self.serial(async move {
            let mut after = 0;
            loop {
                let page = this.page_inner(after, 256).await?;
                if page.is_empty() {
                    break;
                }
                for item in page {
                    after = item.ordinal;
                    if item.phase != Phase::Protected {
                        continue;
                    }
                    let result = this.lease.transfer_protected(&item.token.id).await;
                    if result.is_err() {
                        let id = item.token.id.clone();
                        let current = this
                            .db(move |connection| {
                                load(connection, item.token.catalog, item.token.root, &id)
                            })
                            .await?;
                        if current.phase == Phase::Protected {
                            this.lease
                                .refresh_transfer_charge(&current.token.id)
                                .await
                                .map_err(AdminError::backend)?;
                        }
                    }
                    match result {
                        Ok(()) => {}
                        Err(super::super::conflicts::Error::Capacity) => {}
                        Err(error) => return Err(AdminError::backend(error).into()),
                    }
                }
            }
            let usage = this.status_inner().await?;
            if usage.entries == 0 && !usage.over_limit() {
                this.resume_inner().await
            } else {
                Ok(usage)
            }
        })
        .await
    }
}
