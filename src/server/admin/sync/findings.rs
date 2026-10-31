use crate::storage::sync_state::observation::{Item, WritebackSummary};
use serde_json::{Value, json};

pub(super) fn value(item: &Item, prefix: &str, quota: Option<&WritebackSummary>) -> Value {
    let (reason, remedy) = match (item.category.as_str(), item.reason.as_str()) {
        ("root-stop", "SourceUnrepresentable") => (
            "SourceUnrepresentable",
            "Inspect source-tree diagnostics; preserve unsafe or unmappable entries, repair their representation and rescan",
        ),
        ("root-stop", "SourceChanging") => (
            "SourceChanging",
            "Finish or stop external writers, restore source access and rescan",
        ),
        ("root-stop", "ConflictCapacity") => (
            "ConflictCapacity",
            "Inspect retained conflicts and protected work, resolve exact revisions within quota, then rescan",
        ),
        ("root-stop", "IoFailure") => (
            "IoFailure",
            "Free disk space or restore access without deleting pending data, inspect retained work and rescan",
        ),
        ("root-stop", "RecoveryRequired") => (
            "RecoveryRequired",
            "Inspect protected workspace entries; preserve them before an exact confirmed recovery discard or registered conflict transfer",
        ),
        ("mapping", "collision") => (
            "Conflict",
            "Preserve the edited file; inspect conflicts or restore the previously exported bytes, then rescan",
        ),
        ("mapping", _) => (
            "Unrepresentable",
            "Use a representable API name or remove the conflicting directory entry, then rescan; preserve source bytes",
        ),
        ("path-stop", "ProtectedDeletion") => (
            "ProtectedDeletion",
            "Release the API hold or retention through a supported operation, then delete the restored file again",
        ),
        ("path-stop", "SourceChanging") => (
            "SourceChanging",
            "Finish or stop the external writer, then rescan or wait for a stable snapshot",
        ),
        ("path-stop", _) => (
            "IoFailure",
            "Restore file permissions or free disk space, preserve pending bytes and request a rescan",
        ),
        ("writeback", _) => (
            "RecoveryRequired",
            "Preserve the workspace and current target; inspect this exact pending operation before recovery or explicit disposal",
        ),
        ("conflict", _) => (
            "Conflict",
            "Inspect the retained conflict revision, then restore with the current expected generation or explicitly discard that revision",
        ),
        _ => (
            "Paused",
            "Inspect server diagnostics, correct the reported condition without removing pending data and request an explicit rescan",
        ),
    };
    let mut value = json!({"finding_id":format!("{}{:02x}{:016x}",&prefix[..64],item.kind,item.ordinal),"category":item.category,
        "reason":reason,"scope":if item.category=="root-stop" {"root"} else {"path"},
        "bucket":item.bucket,"object":item.object,"bucket_incarnation":item.bucket_incarnation,"generation":item.generation,
        "owner_project":item.owner_project,"detail_code":item.reason,"remedy":remedy,"operation_id":item.operation_id,
        "conflict_id":item.conflict_id,"conflict_revision":item.conflict_revision});
    if let Some(recovery) = &item.recovery {
        value["recovery_id"] = json!(item.operation_id);
        value["recovery_revision"] = json!(recovery.recovery_revision);
        value["target_identity"] = json!(recovery.target_identity);
        value["phase"] = json!(recovery.phase);
        value["retained_location"] = json!("workspace");
        value["retained_bytes"] = json!(recovery.retained_bytes);
        value["hashes"] = json!(recovery.hashes);
        value["over_limit"] = json!(quota.is_some_and(|quota| quota.over_limit));
    }
    value
}
