//! The path stops that what the tree holds at a path decides.

use super::Ingest;
use crate::{
    storage::{
        admin::ObjectName,
        sync_state::{
            Ordinal, SyncBinding,
            observation::{PathStop, TransientStop},
            root::RootLease,
        },
    },
    sync::{
        SyncError,
        deletion::Presence,
        interrupt::{Interrupt, Interrupted},
        mapping,
    },
};

/// The path stops that what the tree holds at a path decides, which a scan clears once the path
/// holds the regular file again or nothing at all. An alias edit goes once the file is back to
/// the bytes the tree tracked or gone, since no edit is left to take in.
const TREE_STOPS: [PathStop; 4] = [
    PathStop::SourceChanging,
    PathStop::IoFailure,
    PathStop::EntryReplaced,
    PathStop::AliasEdited,
];

pub(super) async fn clear_missing_observations(
    root: &RootLease,
    interrupt: Interrupt<'_>,
) -> Result<(), SyncError> {
    let naming = root.naming()?;
    let lookup = root.lookup()?;
    let mut after = Ordinal::START;
    while interrupt.check() == Interrupted::No {
        let page = root.transient_stop_page(after).await?;
        if page.is_empty() {
            break;
        }
        for TransientStop {
            ordinal,
            binding,
            name,
        } in page
        {
            after = ordinal;
            if let mapping::PathMapping::File(path) =
                mapping::name_to_path(binding.bucket(), &name, naming)
                && let Some(path) = path.to_str()
                && let Presence::Absent { .. } | Presence::Respelled =
                    crate::sync::deletion::presence(root, &lookup, path.to_owned()).await?
            {
                root.clear_path_stops(binding, name, &TREE_STOPS).await?;
            }
        }
    }
    Ok(())
}

impl Ingest<'_> {
    pub(super) async fn clear_observation(
        &self,
        binding: SyncBinding,
        name: ObjectName,
    ) -> Result<(), SyncError> {
        self.root
            .clear_path_stops(binding, name, &TREE_STOPS)
            .await?;
        Ok(())
    }
}
