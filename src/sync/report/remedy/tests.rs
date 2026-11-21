use super::*;
use crate::sync::report::command::AwaitingSet;

/// The remedy of a set waiting for a decision counts the missing files against the tracked
/// ones, and names the inspection first, then a failed mount to fix before anything else,
/// then the ways out that keep the objects, rejecting and restoring by hand, before the
/// approval that deletes them; it says which files call for which decision. Without a set at
/// hand it names the same steps without a revision.
#[test]
fn a_set_awaiting_a_decision_names_the_ways_that_keep_the_objects_first() {
    let set = AwaitingSet {
        revision: OpaqueId::SAMPLE,
        missing: 150,
        tracked: 200,
        cause: None,
    };
    let revision = set.revision.to_hex();
    let remedy = deletion_approval_remedy(AtHand {
        deletion: Some(set),
        ..AtHand::default()
    });
    assert!(
        remedy.starts_with("150 of the 200 files that synchronization tracked are missing"),
        "{remedy}"
    );
    let order = |remedy: &str, steps: &[String]| {
        let at: Vec<_> = steps
            .iter()
            .map(|step| {
                remedy
                    .find(step.as_str())
                    .unwrap_or_else(|| panic!("{step}: {remedy}"))
            })
            .collect();
        assert!(at.is_sorted(), "{remedy}");
    };
    let steps = [
        Advised::Deletions(Some(&revision)).to_string(),
        "if a mount failed, fix the mount and restart the server".to_owned(),
        "if the files went by mistake, reject".to_owned(),
        Advised::Decide(Decision::Reject, &revision).to_string(),
        format!(
            "restore the files yourself and wait again: {}",
            Advised::Wait
        ),
        "if the files were deleted on purpose, approve".to_owned(),
        Advised::Decide(Decision::Approve, &revision).to_string(),
    ];
    order(&remedy, &steps);
    let one = deletion_approval_remedy(AtHand {
        deletion: Some(AwaitingSet {
            missing: 1,
            tracked: 1,
            ..set
        }),
        ..AtHand::default()
    });
    assert!(
        one.starts_with("1 of the 1 file that synchronization tracked is missing"),
        "{one}"
    );
    let without = deletion_approval_remedy(AtHand::default());
    order(
        &without,
        &[
            Advised::Deletions(None).to_string(),
            "fix the mount".to_owned(),
            "reject the set".to_owned(),
            format!("wait again: {}", Advised::Wait),
            "approve the set".to_owned(),
        ],
    );
}

/// The advice on a set that waits opens with the one sentence that says why it waits, when
/// the inventory knew: a bucket directory missing or showing none of its files, named, the
/// threshold, or files of an earlier undecided set, which also says that files deleted
/// meanwhile joined it. It states what was seen, never that the files were deleted, and the
/// counts and commands follow unchanged.
#[test]
fn a_set_awaiting_a_decision_says_first_why_it_waits() {
    let bucket = crate::storage::admin::BucketName::new("photos").unwrap();
    let without = deletion_approval_remedy(AtHand {
        deletion: Some(AwaitingSet {
            revision: OpaqueId::SAMPLE,
            missing: 2,
            tracked: 2,
            cause: None,
        }),
        ..AtHand::default()
    });
    for (cause, opening) in [
        (
            WaitCause::Bucket {
                bucket: bucket.clone(),
                gone: BucketGone::Missing,
            },
            "The directory of bucket photos is missing, as it is when a mount did not come up,",
        ),
        (
            WaitCause::Bucket {
                bucket,
                gone: BucketGone::Emptied,
            },
            "The directory of bucket photos has shown none of its tracked files since the server started, as it does when a mount came up empty,",
        ),
        (
            WaitCause::Threshold,
            "At least 100 tracked files went missing",
        ),
        (
            WaitCause::Held,
            "The set holds files of an earlier deletion set that nobody approved or rejected, so it waits too; files deleted while a set waits join it",
        ),
    ] {
        let remedy = deletion_approval_remedy(AtHand {
            deletion: Some(AwaitingSet {
                revision: OpaqueId::SAMPLE,
                missing: 2,
                tracked: 2,
                cause: Some(&cause),
            }),
            ..AtHand::default()
        });
        assert!(remedy.starts_with(opening), "{remedy}");
        assert_eq!(
            remedy.matches(". ").count(),
            without.matches(". ").count() + 1
        );
        assert!(remedy.ends_with(&without), "{remedy}");
        assert!(!remedy.contains("deleted while the server"), "{remedy}");
    }
}

/// A refused decision names what became of its set and the command that follows: the
/// status or the wait for a set decided already, the same decision again for one an API
/// write raced, and the status when nothing waits. While another set waits, it counts that
/// set's missing files and names its view and the failed mount to fix first, without saying
/// why the refused revision is not that set; a refused rejection names the rejection of
/// that set, and a refused approval names no approval at all, so a revision that was never
/// the inspected one cannot become a ready command that deletes. A refusal never names the
/// other decision. The sample identifier advice renders is the revision the command samples.
#[test]
fn a_refused_decision_names_what_became_of_its_set() {
    assert_eq!(
        OpaqueId::SAMPLE.to_hex(),
        crate::sync::report::catalog::SAMPLE_DELETION_REVISION
    );
    let revision: OpaqueId = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let current: OpaqueId = "fedcba9876543210fedcba9876543210".parse().unwrap();
    let replaced = Refusal::Replaced {
        revision: current,
        missing: 650,
        tracked: 800,
    };
    for decision in Decision::ALL {
        let refused = stale_decision_remedy(decision, revision, replaced);
        for part in [
                "it is not the deletion set that waits for a decision now (it was replaced, decided before, taken before a restart, or not issued by this server)".to_owned(),
                format!("Deletion set {current} waits instead, and 650 of the 800 files that synchronization tracked are missing"),
                format!("gcs-mimic sync status --view deletions --revision {current} --endpoint URL;"),
                "if a mount failed, fix the mount and restart the server".to_owned(),
            ] {
                assert!(refused.contains(&part), "{decision:?}: {refused}");
            }
        assert!(!refused.contains("replaced it"), "{decision:?}: {refused}");
    }
    let approval = stale_decision_remedy(Decision::Approve, revision, replaced);
    assert!(!approval.contains("gcs-mimic sync approve"), "{approval}");
    assert!(
        approval.ends_with(
            "approve that revision only after checking that it holds the files you meant to delete"
        ),
        "{approval}"
    );
    let rejection = stale_decision_remedy(Decision::Reject, revision, replaced);
    assert!(
            rejection.ends_with(&format!(
                "to write its files back from the objects: gcs-mimic sync reject --deletion-set {current} --endpoint URL"
            )),
            "{rejection}"
        );
    for decision in Decision::ALL {
        let (name, past) = (decision.as_str(), decision.past());
        let same = |set: OpaqueId| format!("gcs-mimic sync {name} --deletion-set {set}");
        for (refusal, parts) in [
            (
                Refusal::Approved,
                vec![
                    "was already approved".to_owned(),
                    "its objects are deleted or being deleted".to_owned(),
                    format!("it was not {past} now"),
                    "gcs-mimic sync status --endpoint URL".to_owned(),
                ],
            ),
            (
                Refusal::Rejected,
                vec![
                    "was already rejected".to_owned(),
                    format!("it was not {past} now"),
                    "gcs-mimic sync wait --endpoint URL;".to_owned(),
                    "if you still want them gone, delete the files again after the write-back ends"
                        .to_owned(),
                ],
            ),
            (
                replaced,
                vec![format!("Deletion set {current} waits instead")],
            ),
            (Refusal::Moved, vec![same(revision)]),
            (
                Refusal::Settled,
                vec![
                    "no deletion set waits for a decision now".to_owned(),
                    "or the inventory that replaces the set has not finished".to_owned(),
                    "gcs-mimic sync wait --endpoint URL".to_owned(),
                ],
            ),
        ] {
            let remedy = stale_decision_remedy(decision, revision, refusal);
            assert!(
                remedy.starts_with(&format!("Deletion set {revision} ")),
                "{remedy}"
            );
            for part in parts {
                assert!(remedy.contains(&part), "{decision:?} {refusal:?}: {remedy}");
            }
            for other in Decision::ALL.into_iter().filter(|other| *other != decision) {
                let command = format!("gcs-mimic sync {}", other.as_str());
                assert!(
                    !remedy.contains(&command),
                    "{decision:?} {refusal:?}: {remedy}"
                );
            }
        }
    }
}
