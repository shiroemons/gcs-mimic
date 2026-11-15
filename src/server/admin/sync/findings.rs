use super::{ObservationCursor, SyncCursor};
use crate::{
    json::JsonError,
    storage::sync_state::{
        OpaqueId,
        observation::{
            FindingCategory, FindingReason, Item, PathStop, PauseReason, WritebackSummary,
        },
    },
    sync::mapping::Reason,
};
use serde_json::{Value, json};

/// Renders one finding. A recovery finding's retained hashes that fail to encode answer like
/// any other unencodable management result.
pub(super) fn value(
    item: &Item,
    prefix: &str,
    quota: Option<&WritebackSummary>,
) -> Result<Value, JsonError> {
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
    // A finding ID is the finding's cursor under a prefix that names only the catalog and the
    // root, not the revision, so the ID stays the same while the finding does.
    let finding_id = SyncCursor::Findings(ObservationCursor {
        category: item.category,
        ordinal: item.ordinal,
    })
    .encode(prefix.get(..64).unwrap_or(prefix));
    let mut value = json!({"finding_id":finding_id,"category":item.category,
        "reason":reason,"scope":if item.category==FindingCategory::RootStop {"root"} else {"path"},
        "bucket":item.bucket,"object":item.object,"bucket_incarnation":item.bucket_incarnation,"generation":item.generation,
        "owner_project":item.owner_project,"detail_code":item.reason,"remedy":remedy,"operation_id":item.operation_id,
        "conflict_id":item.conflict_id,"conflict_revision":item.conflict_revision});
    if let Some(recovery) = &item.recovery
        && let Some(value) = value.as_object_mut()
    {
        value.insert(
            "recovery_id".into(),
            Value::from(item.operation_id.map(OpaqueId::to_hex)),
        );
        value.insert(
            "recovery_revision".into(),
            Value::from(recovery.recovery_revision.to_hex()),
        );
        value.insert(
            "target_identity".into(),
            Value::from(recovery.target_identity.to_hex()),
        );
        value.insert("phase".into(), Value::from(recovery.phase.as_str()));
        value.insert("retained_location".into(), Value::from("workspace"));
        value.insert(
            "retained_bytes".into(),
            Value::from(recovery.retained_bytes),
        );
        value.insert("hashes".into(), super::super::encode(&recovery.hashes)?);
        value.insert(
            "over_limit".into(),
            Value::from(quota.is_some_and(|quota| quota.over_limit)),
        );
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sync_state::{
        Ordinal, conflicts::RecoveryPhase, observation::RecoveryDetails, writeback::Phase,
    };

    fn id(digit: char) -> OpaqueId {
        digit.to_string().repeat(32).parse().unwrap()
    }

    fn recovery_finding(hashes: serde_json::Value) -> Item {
        Item {
            ordinal: Ordinal::try_from(7).unwrap(),
            category: FindingCategory::Writeback,
            bucket: Some("bucket".into()),
            object: Some("object".into()),
            bucket_incarnation: None,
            generation: Some(5),
            reason: FindingReason::Writeback(Phase::Protected),
            operation_id: Some(id('a')),
            owner_project: Some("project".into()),
            recovery: Some(RecoveryDetails {
                recovery_revision: id('b'),
                target_identity: id('c'),
                phase: RecoveryPhase::Protected,
                retained_bytes: 18,
                hashes: serde_json::from_value(hashes).unwrap(),
            }),
            conflict_id: None,
            conflict_revision: None,
        }
    }

    /// A protected write-back finding lists its recovery fields after the common ones.
    #[test]
    fn recovery_findings_render_their_workspace_details() {
        let md5: Vec<u8> = (0..16).collect();
        let hashes = json!({"md5": md5, "crc32c": 4_294_967_295_u32});
        let quota = WritebackSummary {
            state: crate::storage::sync_state::writeback::Stop::Ready,
            reserved_bytes: 0,
            retained_bytes: 18,
            entries: 1,
            protected_entries: 1,
            max_bytes: 10,
            max_entries: 10,
            over_limit: true,
        };
        let a = "a".repeat(32);
        let b = "b".repeat(32);
        let c = "c".repeat(32);
        let expected = format!(
            r#"{{"finding_id":"prefix010000000000000007","category":"writeback","reason":"RecoveryRequired","scope":"path","bucket":"bucket","object":"object","bucket_incarnation":null,"generation":5,"owner_project":"project","detail_code":"protected","remedy":"Preserve the workspace and current target; inspect this exact pending operation before recovery or explicit disposal","operation_id":"{a}","conflict_id":null,"conflict_revision":null,"recovery_id":"{a}","recovery_revision":"{b}","target_identity":"{c}","phase":"protected","retained_location":"workspace","retained_bytes":18,"hashes":{{"md5":[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15],"crc32c":4294967295}},"over_limit":true}}"#
        );
        let rendered = value(&recovery_finding(hashes), "prefix", Some(&quota)).unwrap();
        assert_eq!(serde_json::to_string(&rendered).unwrap(), expected);
        let without = value(&recovery_finding(Value::Null), "prefix", None).unwrap();
        assert_eq!(without["hashes"], Value::Null);
        assert_eq!(without["over_limit"], false);
    }
}
