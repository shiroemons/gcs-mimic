//! Publishing the staged bytes of a candidate and tracking its file.

#[cfg(test)]
use super::checkpoint;
use super::{Candidate, Ingest};
use crate::{
    storage::{
        admin::{
            AdminError, BatchPublication, Generation, GenerationCondition, Publication,
            PublishMode, StagedObject,
        },
        sync_state::{SyncFingerprint, SyncUpdate},
    },
    sync::SyncError,
};

impl Ingest<'_> {
    /// Tracks the candidate's file, which holds the bytes of the live generation `generation`.
    pub(super) async fn acknowledge_identical(
        &mut self,
        candidate: &Candidate,
        generation: Generation,
        fingerprint: SyncFingerprint,
    ) -> Result<(), SyncError> {
        let Some(durable) = self
            .make_durable(
                &candidate.key,
                candidate.identity,
                &candidate.binding,
                &candidate.name,
            )
            .await?
        else {
            return Ok(());
        };
        let update = SyncUpdate::new(candidate.binding.clone(), durable, fingerprint)?;
        match self
            .catalog
            .sync_state()
            .acknowledge(&candidate.name, generation, update)
            .await
        {
            Ok(()) => {
                self.summary.identical += 1;
                #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                self.reached(crate::storage::fault::Step::SyncSourceRecorded)
                    .await?;
            }
            Err(AdminError::Precondition | AdminError::NotFound) => {
                self.summary.count_deferred(&candidate.path);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// The publication of the file at `path` as an object's next generation under `condition`,
    /// with the content type its name suggests unless the object has one.
    pub(super) fn publication(
        &self,
        path: &std::path::Path,
        condition: GenerationCondition,
    ) -> Publication {
        Publication {
            content_type: self
                .config
                .seed_content_type
                .for_name(
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default(),
                )
                .into(),
            metadata: Default::default(),
            expected: Default::default(),
            condition: Some(condition),
            max_bytes: self.config.max_object_bytes,
            mode: PublishMode::InheritAndSkipIfSameHash,
        }
    }

    /// Publishes the candidate's staged bytes as the object's next generation and tracks its file.
    pub(super) async fn publish(
        &mut self,
        candidate: Candidate,
        staged: StagedObject,
        fingerprint: SyncFingerprint,
    ) -> Result<(), SyncError> {
        #[cfg(test)]
        checkpoint("publish").await;
        let Candidate {
            path,
            key,
            identity,
            bucket,
            name,
            binding,
            live,
            ..
        } = candidate;
        let condition = live
            .as_ref()
            .map_or(Ok(GenerationCondition::Absent), |record| {
                Generation::new(record.generation).map(GenerationCondition::Is)
            })?;
        let Some(durable) = self.make_durable(&key, identity, &binding, &name).await? else {
            staged.discard().await?;
            return Ok(());
        };
        let update = SyncUpdate::new(binding, durable, fingerprint)?;
        self.root.revalidate().await?;
        let result = self
            .admin
            .publish_objects(vec![BatchPublication {
                bucket,
                name,
                request: self.publication(&path, condition),
                staged: staged.with_sync_state(update),
            }])
            .await?;
        for result in result.results {
            match result {
                Ok(_) => {
                    self.summary.ingested += 1;
                    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                    self.reached(crate::storage::fault::Step::SyncSourceRecorded)
                        .await?;
                }
                Err(AdminError::Precondition | AdminError::NotFound | AdminError::Hold) => {
                    self.summary.count_deferred(&path)
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
