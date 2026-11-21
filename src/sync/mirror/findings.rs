//! The alias of an object whose file stands for another object too.

use crate::storage::sync_state::findings::alias::{AliasHold, Marked};
use crate::storage::sync_state::observation::Transition;
use crate::sync::export::LiveObject;
use crate::{
    diagnostics::{self, LogLevel},
    object::ObjectRecord,
    storage::{
        admin::{BucketName, Generation},
        sync_state::SyncBinding,
    },
    sync::{SyncError, error::superseded, report::remedy::Correction},
};

/// What an object's alias does to its export.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sync) enum Freeze {
    /// No alias names the object or its file.
    NotAliased,
    /// Another live object stands for the same file, so it stays as it is.
    Held,
    /// The alias ended, and the export that settles the file removes its finding.
    Released,
}

impl LiveObject<'_> {
    /// Records that the tree holds this object's file under `spelling`, an alias of its path,
    /// for this object and the one tracked at `spelling`, and reports each that became frozen
    /// now. A generation that changed or went since this export read it is the next replay's.
    pub(in crate::sync) async fn alias(
        &mut self,
        binding: &SyncBinding,
        record: &ObjectRecord,
        spelling: String,
    ) -> Result<(), SyncError> {
        let generation = Generation::new(record.generation)?;
        let recorded = match self
            .root
            .record_alias(
                binding.clone(),
                self.name.clone(),
                generation,
                spelling.clone(),
            )
            .await
        {
            Ok(recorded) => recorded,
            Err(error) if superseded(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        self.summary.count_unmapped_object();
        let partner = recorded.partner.as_ref().map(|(name, _)| name.as_str());
        if recorded.own == Marked::New {
            frozen(self.bucket, self.name.as_str(), &spelling, partner);
        }
        if let Some((name, Marked::New)) = &recorded.partner {
            frozen(
                self.bucket,
                name.as_str(),
                &spelling,
                Some(self.name.as_str()),
            );
        }
        Ok(())
    }

    /// Whether an alias keeps this object out of the tree, decided before its file is read, and
    /// reports an alias that ended now.
    pub(in crate::sync) async fn hold_alias(
        &mut self,
        binding: &SyncBinding,
        record: &ObjectRecord,
    ) -> Result<Freeze, SyncError> {
        let generation = Generation::new(record.generation)?;
        let hold = match self
            .root
            .hold_alias(binding.clone(), self.name.clone(), generation)
            .await
        {
            Ok(hold) => hold,
            // The binding or the object changed meanwhile; the replay of that change observes
            // the object.
            Err(error) if superseded(&error) => return Ok(Freeze::Held),
            Err(error) => return Err(error.into()),
        };
        Ok(match hold {
            AliasHold::NotAliased => Freeze::NotAliased,
            AliasHold::Held => {
                self.summary.count_unmapped_object();
                Freeze::Held
            }
            AliasHold::Released {
                spelling,
                transition,
            } => {
                if transition == Transition::New {
                    diagnostics::emit(
                        LogLevel::Info,
                        "sync_alias_released",
                        serde_json::json!({
                            "bucket":self.bucket.as_str(),"object":self.name.as_str(),"path":spelling,
                            "next":RELEASED
                        }),
                    );
                }
                Freeze::Released
            }
        })
    }
}

/// What follows the end of an alias, which `sync_alias_released` reports.
const RELEASED: &str = "Only this object is left of the alias. It gets its own file under its own name, after an edit kept out while the file was frozen is uploaded for it, and its alias finding goes once that is done";

/// Reports once that the file at `path` stands for `object` and, where the tree tracks it,
/// `partner`, and stays frozen.
fn frozen(bucket: &BucketName, object: &str, path: &str, partner: Option<&str>) {
    diagnostics::emit(
        LogLevel::Warn,
        "sync_alias_frozen",
        serde_json::json!({
            "bucket":bucket.as_str(),"object":object,"path":path,"partner":partner,
            "remedy":Correction::AliasedObjects.remedy()
        }),
    );
}
