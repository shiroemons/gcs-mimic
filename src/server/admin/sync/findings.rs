use crate::{
    storage::sync_state::observation::{
        FindingCategory, FindingReason, Item, PathStop, PauseReason, WritebackSummary,
    },
    sync::mapping::Reason,
};
use serde_json::{Value, json};

pub(super) fn value(item: &Item, prefix: &str, quota: Option<&WritebackSummary>) -> Value {
    let (reason, remedy) = match &item.reason {
        FindingReason::RootStop(PauseReason::SourceUnrepresentable) => (
            "SourceUnrepresentable",
            "Inspect source-tree diagnostics; preserve unsafe or unmappable entries, repair their representation and rescan",
        ),
        FindingReason::RootStop(PauseReason::SourceChanging) => (
            "SourceChanging",
            "Finish or stop external writers, restore source access and rescan",
        ),
        FindingReason::RootStop(PauseReason::ConflictCapacity) => (
            "ConflictCapacity",
            "Inspect retained conflicts and protected work, resolve exact revisions within quota, then rescan",
        ),
        FindingReason::RootStop(PauseReason::IoFailure) => (
            "IoFailure",
            "Free disk space or restore access without deleting pending data, inspect retained work and rescan",
        ),
        FindingReason::RootStop(PauseReason::RecoveryRequired) => (
            "RecoveryRequired",
            "Inspect protected workspace entries; preserve them before an exact confirmed recovery discard or registered conflict transfer",
        ),
        FindingReason::Mapping(Reason::Collision) => (
            "Conflict",
            "Preserve the edited file; inspect conflicts or restore the previously exported bytes, then rescan",
        ),
        FindingReason::Mapping(_) => (
            "Unrepresentable",
            "Use a representable API name or remove the conflicting directory entry, then rescan; preserve source bytes",
        ),
        FindingReason::PathStop(PathStop::ProtectedDeletion) => (
            "ProtectedDeletion",
            "Release the API hold or retention through a supported operation, then delete the restored file again",
        ),
        FindingReason::PathStop(PathStop::SourceChanging) => (
            "SourceChanging",
            "Finish or stop the external writer, then rescan or wait for a stable snapshot",
        ),
        FindingReason::PathStop(PathStop::IoFailure) => (
            "IoFailure",
            "Restore file permissions or free disk space, preserve pending bytes and request a rescan",
        ),
        FindingReason::Writeback(_) => (
            "RecoveryRequired",
            "Preserve the workspace and current target; inspect this exact pending operation before recovery or explicit disposal",
        ),
        FindingReason::Conflict(_) => (
            "Conflict",
            "Inspect the retained conflict revision, then restore with the current expected generation or explicitly discard that revision",
        ),
        FindingReason::RootStop(PauseReason::ObserverFailure) => (
            "Paused",
            "Inspect server diagnostics, correct the reported condition without removing pending data and request an explicit rescan",
        ),
    };
    let mut value = json!({"finding_id":format!("{}{:02x}{:016x}",prefix.get(..64).unwrap_or(prefix),item.kind,item.ordinal),"category":item.category,
        "reason":reason,"scope":if item.category==FindingCategory::RootStop {"root"} else {"path"},
        "bucket":item.bucket,"object":item.object,"bucket_incarnation":item.bucket_incarnation,"generation":item.generation,
        "owner_project":item.owner_project,"detail_code":item.reason,"remedy":remedy,"operation_id":item.operation_id,
        "conflict_id":item.conflict_id,"conflict_revision":item.conflict_revision});
    if let Some(recovery) = &item.recovery
        && let Some(value) = value.as_object_mut()
    {
        value.insert("recovery_id".into(), json!(item.operation_id));
        value.insert(
            "recovery_revision".into(),
            json!(recovery.recovery_revision),
        );
        value.insert("target_identity".into(), json!(recovery.target_identity));
        value.insert("phase".into(), json!(recovery.phase));
        value.insert("retained_location".into(), json!("workspace"));
        value.insert("retained_bytes".into(), json!(recovery.retained_bytes));
        value.insert("hashes".into(), json!(recovery.hashes));
        value.insert(
            "over_limit".into(),
            json!(quota.is_some_and(|quota| quota.over_limit)),
        );
    }
    value
}
