//! SetStorageClass shares `evaluate` with Delete. These cases rerun the condition boundaries that
//! the Delete tests pin with a transition action, so a condition family cannot apply to one
//! action and silently not the other.

use super::*;

/// A colder class than the default `Standard` fact, so every matching rule is a valid transition.
const TARGET: StorageClass = StorageClass::Coldline;
const TRANSITION: Option<PlannedAction> = Some(PlannedAction::SetStorageClass(TARGET));

fn transition(condition: Value) -> LifecycleRule {
    rules(&[json!({
        "action": {"type": "SetStorageClass", "storageClass": TARGET},
        "condition": condition,
    })])
    .remove(0)
}

/// Waits until `due` one microsecond early, then transitions at and after it.
fn assert_transition_boundary(condition: Value, facts: &VersionFacts<'_>, due: SystemTime) {
    let rules = [transition(condition)];
    assert_evaluation(&rules, facts, due - MICROSECOND, None, Some(due));
    for now in [due, due + MICROSECOND] {
        assert_evaluation(&rules, facts, now, TRANSITION, None);
    }
}

/// Matches or not immediately, with no wakeup, as calendar and count conditions do.
fn assert_transition_now(condition: Value, facts: &VersionFacts<'_>, matches: bool) {
    assert_evaluation(
        &[transition(condition)],
        facts,
        created(facts),
        matches.then_some(PlannedAction::SetStorageClass(TARGET)),
        None,
    );
}

/// Each before-date field matches only an instant strictly earlier than the date's UTC midnight.
fn assert_strict_before_date(field: &str) {
    let midnight = timestamp("2026-02-01T00:00:00Z");
    let just_before = UnixNanos::new(midnight.secs() - 1, 999_999_000).unwrap();
    for (instant, matches) in [
        (just_before, true),
        (midnight, false),
        (midnight.checked_add(MICROSECOND).unwrap(), false),
    ] {
        let mut facts = version();
        match field {
            "createdBefore" => facts.time_created = instant,
            "customTimeBefore" => facts.custom_time = Some(instant),
            "noncurrentTimeBefore" => facts.noncurrent_time = Some(instant),
            _ => unreachable!("test fields are exhaustive"),
        }
        assert_evaluation(
            &[transition(json!({field: "2026-02-01"}))],
            &facts,
            UNIX_EPOCH,
            matches.then_some(PlannedAction::SetStorageClass(TARGET)),
            None,
        );
    }
}

#[test]
fn transitions_apply_age_and_creation_date_conditions() {
    let facts = version();
    assert_transition_boundary(json!({"age":10}), &facts, created(&facts) + 10 * DAY);
    assert_transition_boundary(json!({"age":0}), &facts, time("2026-01-11T00:00:00Z"));
    assert_strict_before_date("createdBefore");
}

#[test]
fn transitions_apply_custom_time_conditions() {
    let mut facts = version();
    let now = created(&facts);
    for days in [0, 2] {
        facts.custom_time = None;
        assert_evaluation(
            &[transition(json!({"daysSinceCustomTime":days}))],
            &facts,
            now,
            None,
            None,
        );
        for custom in [now - 3 * DAY, now, now + 3 * DAY] {
            facts.custom_time = Some(custom.into());
            assert_transition_boundary(
                json!({"daysSinceCustomTime":days}),
                &facts,
                custom + days * DAY,
            );
        }
    }
    facts.custom_time = None;
    assert_transition_now(json!({"customTimeBefore":"2026-02-01"}), &facts, false);
    assert_strict_before_date("customTimeBefore");
}

#[test]
fn transitions_apply_version_state_count_and_noncurrent_time_conditions() {
    let mut facts = version();
    let now = created(&facts);
    for live in [false, true] {
        for noncurrent in [None, Some(facts.time_created)] {
            facts.noncurrent_time = noncurrent;
            assert_transition_now(json!({"isLive":live}), &facts, live == noncurrent.is_none());
        }
    }
    for newer in [0, 1, 2] {
        facts.newer_versions = newer;
        for minimum in [0, 1, 2, 3] {
            assert_transition_now(
                json!({"numNewerVersions":minimum}),
                &facts,
                newer >= minimum,
            );
        }
    }
    for days in [0, 3] {
        facts.noncurrent_time = None;
        assert_evaluation(
            &[transition(json!({"daysSinceNoncurrentTime":days}))],
            &facts,
            now + 10 * DAY,
            None,
            None,
        );
        facts.noncurrent_time = Some((now + DAY).into());
        assert_transition_boundary(
            json!({"daysSinceNoncurrentTime":days}),
            &facts,
            now + (days + 1) * DAY,
        );
    }
    assert_strict_before_date("noncurrentTimeBefore");
}

#[test]
fn transitions_apply_strict_size_conditions() {
    let mut facts = version();
    for (condition, cases) in [
        (
            json!({"sizeAboveBytes":"100"}),
            [(99, false), (100, false), (101, true)],
        ),
        (
            json!({"sizeBelowBytes":100}),
            [(99, true), (100, false), (101, false)],
        ),
        (
            json!({"sizeAboveBytes":0,"sizeBelowBytes":"5497558138880"}),
            [(0, false), (1, true), (5 * 1024_u64.pow(4), false)],
        ),
    ] {
        for (size, matches) in cases {
            facts.size = size;
            assert_transition_now(condition.clone(), &facts, matches);
        }
    }
}
