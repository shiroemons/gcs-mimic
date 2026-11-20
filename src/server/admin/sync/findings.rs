use super::{ObservationCursor, SyncCursor};
use crate::{
    api_error::ApiError,
    storage::sync_state::{
        OpaqueId,
        findings::FindingKind,
        observation::{
            FindingCategory, FindingReason, Item, PathStop, PauseReason, WritebackSummary,
        },
    },
    sync::{
        advice::AtHand,
        control::{self, Correction},
        mapping::Reason,
    },
};
use serde_json::{Value, json};
use std::borrow::Cow;

/// Renders one finding of the page at the findings revision `at_hand`, which a root stop's
/// remedy names. A recovery finding's retained hashes that fail to encode answer like any
/// other unencodable management result.
pub(super) fn value(
    item: &Item,
    prefix: &str,
    quota: Option<&WritebackSummary>,
    at_hand: AtHand<'_>,
) -> Result<Value, ApiError> {
    let (reason, remedy): (&str, Cow<'static, str>) = match &item.reason {
        FindingReason::RootStop(pause) => (
            match pause {
                PauseReason::ObserverFailure => "Paused",
                PauseReason::ConflictCapacity
                | PauseReason::IoFailure
                | PauseReason::RecoveryRequired
                | PauseReason::SourceChanging
                | PauseReason::SourceUnrepresentable => pause.as_str(),
            },
            control::pause_remedy(*pause, at_hand).into(),
        ),
        FindingReason::Mapping(FindingKind::ExportConflict) => (
            "Conflict",
            Correction::ExportConflict.remedy().into(),
        ),
        // The row cannot say whether an edited file exists, so the remedy asks for the
        // observation that tells, not for preserving a file that may not be there.
        FindingReason::Mapping(FindingKind::UnclassifiedCollision) => (
            "Conflict",
            control::unclassified_collision_remedy().into(),
        ),
        // A name collision is a name the tree cannot hold, like the other mapping reasons; only
        // an export that met an edited file is a conflict.
        FindingReason::Mapping(FindingKind::Unmapped(
            Reason::InvalidName
            | Reason::HostRestriction
            | Reason::Reserved
            | Reason::Ignored
            | Reason::Collision
            | Reason::UnsafeEntry,
        )) => (
            "Unrepresentable",
            Correction::UnrepresentableName.remedy().into(),
        ),
        // The API name is fine here: only the entries in the directory keep the file out.
        FindingReason::Mapping(FindingKind::Unmapped(Reason::DirectoryOccupied)) => (
            "Unrepresentable",
            crate::sync::control::occupied_directory_remedy().into(),
        ),
        // Renaming the entry would only make it the file of the other object, so the remedy
        // asks for the API change that ends the alias.
        FindingReason::Mapping(FindingKind::Unmapped(Reason::Alias)) => (
            "Unrepresentable",
            Correction::AliasedObjects.remedy().into(),
        ),
        FindingReason::PathStop(PathStop::ProtectedDeletion) => (
            "ProtectedDeletion",
            "Release the API hold or retention through a supported operation, then delete the restored file again".into(),
        ),
        FindingReason::PathStop(PathStop::SourceChanging) => (
            "SourceChanging",
            Correction::ChangingPath.remedy().into(),
        ),
        FindingReason::PathStop(PathStop::IoFailure) => (
            "IoFailure",
            Correction::PathIoFailure.remedy().into(),
        ),
        FindingReason::PathStop(PathStop::EntryReplaced) => (
            "EntryReplaced",
            Correction::ReplacedEntry.remedy().into(),
        ),
        FindingReason::PathStop(PathStop::AliasEdited) => (
            "AliasEdited",
            Correction::EditedAlias.remedy().into(),
        ),
        FindingReason::Writeback(_) => (
            "RecoveryRequired",
            "Preserve the workspace and current target; inspect this exact pending operation before recovery or explicit disposal".into(),
        ),
        FindingReason::Conflict(_) => (
            "Conflict",
            "Inspect the retained conflict revision, then restore with the current expected generation or explicitly discard that revision".into(),
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
    // Only an alias finding has a file under another spelling than its object's own path.
    if let Some(tree_path) = &item.tree_path
        && let Some(value) = value.as_object_mut()
    {
        value.insert("tree_path".into(), Value::from(tree_path.as_str()));
    }
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
            tree_path: None,
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
        let rendered = value(
            &recovery_finding(hashes),
            "prefix",
            Some(&quota),
            AtHand::default(),
        )
        .unwrap();
        assert_eq!(serde_json::to_string(&rendered).unwrap(), expected);
        let without = value(
            &recovery_finding(Value::Null),
            "prefix",
            None,
            AtHand::default(),
        )
        .unwrap();
        assert_eq!(without["hashes"], Value::Null);
        assert_eq!(without["over_limit"], false);
    }

    fn mapping_finding(kind: FindingKind) -> Item {
        Item {
            ordinal: Ordinal::try_from(3).unwrap(),
            category: FindingCategory::Mapping,
            bucket: Some("bucket".into()),
            object: Some("object".into()),
            bucket_incarnation: None,
            generation: Some(5),
            reason: FindingReason::Mapping(kind),
            operation_id: None,
            owner_project: Some("project".into()),
            recovery: None,
            conflict_id: None,
            conflict_revision: None,
            tree_path: None,
        }
    }

    /// Only an export that met an edited file is a conflict; a name collision is a name the
    /// tree cannot hold, like the other mapping reasons.
    #[test]
    fn export_conflicts_render_as_conflicts_and_name_collisions_as_unrepresentable_names() {
        for (kind, rendered, detail) in [
            (FindingKind::ExportConflict, "Conflict", "export-conflict"),
            (
                FindingKind::Unmapped(Reason::Collision),
                "Unrepresentable",
                "collision",
            ),
            (
                FindingKind::Unmapped(Reason::InvalidName),
                "Unrepresentable",
                "invalid-name",
            ),
        ] {
            let value = value(&mapping_finding(kind), "prefix", None, AtHand::default()).unwrap();
            assert_eq!(
                (&value["reason"], &value["detail_code"], &value["scope"]),
                (&json!(rendered), &json!(detail), &json!("path")),
                "{detail}"
            );
        }
    }

    /// A collision carried over from schema 57 may hide an edited file, so it stays a
    /// conflict, but its remedy asks for the observation that classifies it rather than for
    /// preserving a file that may not exist.
    #[test]
    fn carried_over_collisions_render_as_conflicts_that_ask_to_observe_the_object_again() {
        let value = value(
            &mapping_finding(FindingKind::UnclassifiedCollision),
            "prefix",
            None,
            AtHand::default(),
        )
        .unwrap();
        assert_eq!(
            (&value["reason"], &value["detail_code"]),
            (&json!("Conflict"), &json!("unclassified-collision"))
        );
        let remedy = value["remedy"].as_str().unwrap();
        assert!(remedy.contains("observed again and classified"), "{remedy}");
        assert!(!remedy.contains("edited file"), "{remedy}");
    }

    /// A directory with entries at the object's file name has its own detail code, and its
    /// remedy asks to empty the directory rather than to rename the object.
    #[test]
    fn an_occupied_directory_renders_its_own_detail_and_asks_to_empty_it() {
        let value = value(
            &mapping_finding(FindingKind::Unmapped(Reason::DirectoryOccupied)),
            "prefix",
            None,
            AtHand::default(),
        )
        .unwrap();
        assert_eq!(
            (&value["reason"], &value["detail_code"]),
            (&json!("Unrepresentable"), &json!("directory-occupied"))
        );
        let remedy = value["remedy"].as_str().unwrap();
        assert!(remedy.contains("Move everything in it out"), "{remedy}");
        assert!(remedy.contains("gcs-mimic sync rescan"), "{remedy}");
        assert!(!remedy.contains("API name"), "{remedy}");
    }

    /// An alias finding names the file both objects share by its spelling in the tree, and its
    /// remedy asks for the API change that ends the alias, since renaming the file would only
    /// make it the other object's. A frozen edit names its own stop. Other findings have no
    /// spelling to show.
    #[test]
    fn an_alias_renders_the_spelling_of_its_file_and_asks_for_an_api_change() {
        let alias = Item {
            tree_path: Some("bucket/Object".into()),
            ..mapping_finding(FindingKind::Unmapped(Reason::Alias))
        };
        let rendered = value(&alias, "prefix", None, AtHand::default()).unwrap();
        assert_eq!(
            (
                &rendered["reason"],
                &rendered["detail_code"],
                &rendered["tree_path"]
            ),
            (
                &json!("Unrepresentable"),
                &json!("alias"),
                &json!("bucket/Object")
            )
        );
        let remedy = rendered["remedy"].as_str().unwrap();
        assert!(remedy.contains("through the API"), "{remedy}");
        assert!(remedy.contains("deleting it deletes neither"), "{remedy}");
        let edited = Item {
            category: FindingCategory::PathStop,
            reason: FindingReason::PathStop(PathStop::AliasEdited),
            ..mapping_finding(FindingKind::ExportConflict)
        };
        let rendered = value(&edited, "prefix", None, AtHand::default()).unwrap();
        assert_eq!(rendered["reason"], json!("AliasEdited"));
        assert!(rendered.get("tree_path").is_none());
    }

    /// A root stop's remedy names the findings view at the revision of the page that lists it,
    /// and a path's correction ends by naming the wait whose pass takes it in.
    #[test]
    fn remedies_name_the_page_revision_and_the_wait_that_takes_a_correction_in() {
        let revision = crate::sync::advice::SAMPLE_FINDINGS_REVISION;
        let at_hand = AtHand {
            findings_revision: Some(revision),
            ..AtHand::default()
        };
        let root_stop = Item {
            category: FindingCategory::RootStop,
            reason: FindingReason::RootStop(PauseReason::RecoveryRequired),
            ..mapping_finding(FindingKind::ExportConflict)
        };
        let rendered = value(&root_stop, "prefix", None, at_hand).unwrap();
        let remedy = rendered["remedy"].as_str().unwrap();
        assert!(
            remedy.contains(&format!(
                "List them: gcs-mimic sync status --view findings --revision {revision} --endpoint URL;"
            )),
            "{remedy}"
        );
        let exported = value(
            &mapping_finding(FindingKind::ExportConflict),
            "prefix",
            None,
            at_hand,
        )
        .unwrap();
        let remedy = exported["remedy"].as_str().unwrap();
        assert!(
            remedy.ends_with("; then wait again: gcs-mimic sync wait --endpoint URL"),
            "{remedy}"
        );
    }
}
