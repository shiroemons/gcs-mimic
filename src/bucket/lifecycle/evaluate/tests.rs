use super::*;
use serde_json::{Value, json};

const MICROSECOND: Duration = Duration::from_micros(1);
const DAY: Duration = Duration::from_secs(86_400);

fn time(value: &str) -> SystemTime {
    let (seconds, nanos) = crate::object::custom_time_key(value).expect("valid test timestamp");
    crate::clock::system_time_from_unix_seconds(seconds)
        .and_then(|time| time.checked_add(Duration::from_nanos(u64::from(nanos))))
        .expect("representable test timestamp")
}

fn version() -> VersionFacts<'static> {
    VersionFacts {
        name: "pictures/paris.jpg",
        size: 100,
        storage_class: StorageClass::Standard,
        time_created: time("2026-01-10T10:00:00.123456Z"),
        custom_time: None,
        noncurrent_time: None,
        newer_versions: 0,
    }
}

fn rules(values: &[Value]) -> Vec<LifecycleRule> {
    LifecycleRule::from_stored(values).expect("valid test rules")
}

fn delete(condition: Value) -> LifecycleRule {
    rules(&[json!({"action":{"type":"Delete"},"condition":condition})]).remove(0)
}

fn assert_evaluation(
    rules: &[LifecycleRule],
    facts: &VersionFacts<'_>,
    now: SystemTime,
    action: Option<PlannedAction>,
    next_due: Option<SystemTime>,
) {
    let result = evaluate(rules, facts, now);
    assert_eq!(result.action, action, "at {now:?}");
    assert_eq!(result.next_due, next_due, "at {now:?}");
}

fn assert_delete_boundary(rules: &[LifecycleRule], facts: &VersionFacts<'_>, due: SystemTime) {
    assert_evaluation(rules, facts, due - MICROSECOND, None, Some(due));
    for now in [due, due + MICROSECOND] {
        assert_evaluation(rules, facts, now, Some(PlannedAction::Delete), None);
    }
}

#[test]
fn age_uses_the_creation_instant_to_the_microsecond() {
    let mut facts = version();
    let rules = [delete(json!({"age":10}))];
    let due = facts.time_created + 10 * DAY;
    assert_delete_boundary(&rules, &facts, due);
    facts.noncurrent_time = Some(facts.time_created + DAY);
    assert_delete_boundary(&rules, &facts, due);
}

#[test]
fn zero_age_waits_for_the_next_utc_midnight_including_midnight_creation() {
    for (created, midnight) in [
        ("2026-01-10T10:00:00.123456Z", "2026-01-11T00:00:00Z"),
        ("2026-01-10T00:00:00Z", "2026-01-11T00:00:00Z"),
        ("2026-01-10T23:59:59.999999Z", "2026-01-11T00:00:00Z"),
        ("1969-12-30T23:59:59.999999Z", "1969-12-31T00:00:00Z"),
        ("1969-12-31T00:00:00Z", "1970-01-01T00:00:00Z"),
        ("1970-01-01T00:00:00Z", "1970-01-02T00:00:00Z"),
    ] {
        let mut facts = version();
        facts.time_created = time(created);
        assert_delete_boundary(&[delete(json!({"age":0}))], &facts, time(midnight));
    }
}

#[test]
fn overflowing_day_counts_never_match_or_schedule_a_deadline() {
    let mut facts = version();
    facts.custom_time = Some(facts.time_created);
    facts.noncurrent_time = Some(facts.time_created);
    for field in ["age", "daysSinceCustomTime", "daysSinceNoncurrentTime"] {
        let rule = delete(json!({field: i32::MAX}));
        assert_evaluation(&[rule], &facts, facts.time_created + DAY, None, None);
    }
    assert!(after_days(facts.time_created, i64::MAX).is_none());
    assert!(after_days(facts.time_created, -1).is_none());
    if let Some(last) = crate::clock::system_time_from_unix_seconds(i64::MAX) {
        assert!(after_days(last, 1).is_none());
        assert!(next_midnight(last).is_none());
    }
}

#[test]
fn deadlines_never_exceed_the_catalog_microsecond_range() {
    let days = i64::MAX / 86_400_000_000;
    let origin = UNIX_EPOCH + Duration::from_micros((i64::MAX % 86_400_000_000) as u64);
    for field in ["age", "daysSinceCustomTime", "daysSinceNoncurrentTime"] {
        let rules = [delete(json!({field:days}))];
        for (start, in_range) in [
            (origin - MICROSECOND, true),
            (origin, true),
            (origin + MICROSECOND, false),
        ] {
            let mut facts = version();
            facts.time_created = start;
            facts.custom_time = Some(start);
            facts.noncurrent_time = Some(start);
            // Some platforms reach their SystemTime limit before the catalog limit.
            let expected = in_range
                .then(|| start.checked_add(Duration::from_secs(days as u64 * 86_400)))
                .flatten();
            assert_evaluation(&rules, &facts, start, None, expected);
            if let Some(deadline) = expected {
                assert_delete_boundary(&rules, &facts, deadline);
            }
        }
    }
    let days_from_today = delete(json!({"age":days}));
    let facts = version();
    assert_evaluation(&[days_from_today], &facts, facts.time_created, None, None);
}

#[test]
fn before_date_conditions_are_strict_at_utc_midnight() {
    let midnight = time("2026-02-01T00:00:00Z");
    for field in ["createdBefore", "customTimeBefore", "noncurrentTimeBefore"] {
        let rules = [delete(json!({field: "2026-02-01"}))];
        for (instant, action) in [
            (midnight - MICROSECOND, Some(PlannedAction::Delete)),
            (midnight, None),
            (midnight + MICROSECOND, None),
        ] {
            let mut facts = version();
            facts.time_created = midnight + DAY;
            facts.custom_time = Some(midnight + 2 * DAY);
            facts.noncurrent_time = Some(midnight + 3 * DAY);
            match field {
                "createdBefore" => facts.time_created = instant,
                "customTimeBefore" => facts.custom_time = Some(instant),
                "noncurrentTimeBefore" => facts.noncurrent_time = Some(instant),
                _ => unreachable!("test fields are exhaustive"),
            }
            assert_evaluation(&rules, &facts, midnight + DAY, action, None);
        }
    }
    let facts = version();
    for field in ["customTimeBefore", "noncurrentTimeBefore"] {
        assert_evaluation(
            &[delete(json!({field:"2026-02-01"}))],
            &facts,
            midnight,
            None,
            None,
        );
    }
}

#[test]
fn date_conditions_cover_leap_days_and_pre_epoch_dates_without_a_wakeup() {
    for date in [
        "0001-01-01",
        "1900-03-01",
        "1969-12-31",
        "2000-02-29",
        "9999-12-31",
    ] {
        let midnight = time(&format!("{date}T00:00:00Z"));
        let mut facts = version();
        let rules = [delete(json!({"createdBefore":date}))];
        facts.time_created = midnight - MICROSECOND;
        assert_evaluation(&rules, &facts, midnight, Some(PlannedAction::Delete), None);
        facts.time_created = midnight;
        assert_evaluation(&rules, &facts, midnight, None, None);
    }
}

#[test]
fn custom_time_days_require_metadata_and_preserve_future_and_past_boundaries() {
    let mut facts = version();
    let now = facts.time_created;
    for days in [0, 2] {
        let rules = [delete(json!({"daysSinceCustomTime":days}))];
        facts.custom_time = None;
        assert_evaluation(&rules, &facts, now, None, None);
        for custom in [now - 3 * DAY, now, now + 3 * DAY] {
            facts.custom_time = Some(custom);
            assert_delete_boundary(&rules, &facts, custom + days * DAY);
        }
    }
}

#[test]
fn noncurrent_days_start_at_the_transition_instant_and_never_match_live_versions() {
    let mut facts = version();
    let now = facts.time_created;
    for days in [0, 3] {
        let rules = [delete(json!({"daysSinceNoncurrentTime":days}))];
        facts.noncurrent_time = None;
        assert_evaluation(&rules, &facts, now + 10 * DAY, None, None);
        facts.noncurrent_time = Some(now + DAY);
        assert_delete_boundary(&rules, &facts, now + (days + 1) * DAY);
    }
}

#[test]
fn newer_version_threshold_counts_live_and_noncurrent_versions_inclusively() {
    for (noncurrent, newer) in [
        (None, 0),
        (Some(version().time_created), 0),
        (Some(version().time_created), 1),
        (Some(version().time_created), 2),
    ] {
        let mut facts = version();
        facts.noncurrent_time = noncurrent;
        facts.newer_versions = newer;
        for minimum in [0, 1, 2, 3] {
            let action = (newer >= minimum).then_some(PlannedAction::Delete);
            assert_evaluation(
                &[delete(json!({"numNewerVersions":minimum}))],
                &facts,
                facts.time_created,
                action,
                None,
            );
        }
    }
}

#[test]
fn live_condition_distinguishes_live_and_noncurrent_facts() {
    let mut facts = version();
    for noncurrent in [None, Some(facts.time_created)] {
        facts.noncurrent_time = noncurrent;
        for live in [false, true] {
            assert_evaluation(
                &[delete(json!({"isLive":live}))],
                &facts,
                facts.time_created,
                (live == noncurrent.is_none()).then_some(PlannedAction::Delete),
                None,
            );
        }
    }
}

#[test]
fn name_conditions_match_any_entry_case_sensitively_and_combine_with_and() {
    let rules = [delete(
        json!({"matchesPrefix":["other/","pictures/"],"matchesSuffix":[".png",".jpg"]}),
    )];
    let mut facts = version();
    for (name, matches) in [
        ("pictures/paris.jpg", true),
        ("pictures/paris.png", true),
        ("other/paris.jpg", true),
        ("Pictures/paris.jpg", false),
        ("pictures/paris.JPG", false),
        ("pictures/paris.gif", false),
        ("/pictures/paris.jpg", false),
        ("bucket/pictures/paris.jpg", false),
    ] {
        facts.name = name;
        assert_evaluation(
            &rules,
            &facts,
            facts.time_created,
            matches.then_some(PlannedAction::Delete),
            None,
        );
    }
}

#[test]
fn storage_class_conditions_match_any_list_entry() {
    let mut facts = version();
    for class in [
        StorageClass::Standard,
        StorageClass::MultiRegional,
        StorageClass::DurableReducedAvailability,
        StorageClass::Nearline,
        StorageClass::Coldline,
        StorageClass::Archive,
    ] {
        facts.storage_class = class;
        assert_evaluation(
            &[delete(
                json!({"matchesStorageClass":["REGIONAL",class.as_str()]}),
            )],
            &facts,
            facts.time_created,
            Some(PlannedAction::Delete),
            None,
        );
        assert_evaluation(
            &[delete(json!({"matchesStorageClass":["REGIONAL"]}))],
            &facts,
            facts.time_created,
            None,
            None,
        );
    }
}

#[test]
fn size_conditions_use_strict_data_byte_comparisons() {
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
            json!({"sizeAboveBytes":99,"sizeBelowBytes":"101"}),
            [(99, false), (100, true), (101, false)],
        ),
        (
            json!({"sizeAboveBytes":0,"sizeBelowBytes":"5497558138880"}),
            [(0, false), (1, true), (5 * 1024_u64.pow(4), false)],
        ),
    ] {
        let rules = [delete(condition)];
        for (size, matches) in cases {
            facts.size = size;
            assert_evaluation(
                &rules,
                &facts,
                facts.time_created,
                matches.then_some(PlannedAction::Delete),
                None,
            );
        }
    }
}

#[test]
fn delete_wins_over_the_lowest_price_matching_storage_class_in_any_order() {
    let facts = version();
    let mut rules = rules(&[
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"isLive":true}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"isLive":true}}),
    ]);
    for _ in 0..2 {
        assert_evaluation(
            &rules,
            &facts,
            facts.time_created,
            Some(PlannedAction::SetStorageClass(StorageClass::Coldline)),
            None,
        );
        rules.reverse();
    }
    rules.push(delete(json!({"isLive":true})));
    for _ in 0..2 {
        assert_evaluation(
            &rules,
            &facts,
            facts.time_created,
            Some(PlannedAction::Delete),
            None,
        );
        rules.reverse();
    }
}

#[test]
fn rules_with_the_same_action_are_or_but_conditions_within_one_rule_are_and() {
    let facts = version();
    let mut rules = [
        delete(json!({"age":1,"matchesPrefix":["absent/"]})),
        delete(json!({"isLive":false})),
    ];
    assert_evaluation(&rules, &facts, facts.time_created, None, None);
    assert_evaluation(&rules, &facts, facts.time_created + 2 * DAY, None, None);
    rules[1] = delete(json!({"age":2}));
    assert_delete_boundary(&rules, &facts, facts.time_created + 2 * DAY);
}

#[test]
fn abort_rules_never_select_an_object_action_or_deadline() {
    let facts = version();
    let rules = rules(&[
        json!({"action":{"type":"AbortIncompleteMultipartUpload"},"condition":{"age":0}}),
        json!({"action":{"type":"AbortIncompleteMultipartUpload"},"condition":{"matchesPrefix":["pictures/"]}}),
    ]);
    for now in [facts.time_created, facts.time_created + 10 * DAY] {
        assert_evaluation(&rules, &facts, now, None, None);
    }
    assert_evaluation(&[], &facts, facts.time_created, None, None);
}

#[test]
fn only_documented_storage_class_transitions_are_selected() {
    use StorageClass::{
        Archive, Coldline, DurableReducedAvailability as Dra, MultiRegional, Nearline, Standard,
    };
    let classes = [Standard, MultiRegional, Dra, Nearline, Coldline, Archive];
    let transitions = [
        (Dra, Nearline),
        (Dra, Coldline),
        (Dra, Archive),
        (Dra, MultiRegional),
        (Standard, Nearline),
        (Standard, Coldline),
        (Standard, Archive),
        (MultiRegional, Nearline),
        (MultiRegional, Coldline),
        (MultiRegional, Archive),
        (Nearline, Coldline),
        (Nearline, Archive),
        (Coldline, Archive),
    ];
    let mut facts = version();
    for current in classes {
        facts.storage_class = current;
        for target in classes {
            let rules = rules(&[
                json!({"action":{"type":"SetStorageClass","storageClass":target.as_str()},"condition":{"age":1}}),
            ]);
            let action = transitions
                .contains(&(current, target))
                .then_some(PlannedAction::SetStorageClass(target));
            assert_evaluation(
                &rules,
                &facts,
                facts.time_created,
                None,
                action.map(|_| facts.time_created + DAY),
            );
            assert_evaluation(&rules, &facts, facts.time_created + DAY, action, None);
        }
    }
}

#[test]
fn reachable_actions_have_distinct_priorities() {
    use StorageClass::{
        Archive, Coldline, DurableReducedAvailability, MultiRegional, Nearline, Standard,
    };
    let classes = [
        Standard,
        MultiRegional,
        DurableReducedAvailability,
        Nearline,
        Coldline,
        Archive,
    ];
    for current in classes {
        let actions = std::iter::once(LifecycleAction::Delete)
            .chain(classes.map(LifecycleAction::SetStorageClass))
            .filter_map(|action| planned_action(action, current))
            .collect::<Vec<_>>();
        for (index, action) in actions.iter().enumerate() {
            assert!(action.priority() < 5);
            for other in &actions[index + 1..] {
                assert_ne!(
                    action.priority(),
                    other.priority(),
                    "{current:?}: {action:?}, {other:?}"
                );
            }
        }
    }
}

#[test]
fn a_future_warmer_class_does_not_schedule_a_wakeup() {
    let facts = version();
    let rules = rules(&[
        json!({"action":{"type":"SetStorageClass","storageClass":"ARCHIVE"},"condition":{"isLive":true}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":1}}),
    ]);
    let due = facts.time_created + DAY;
    for now in [due - MICROSECOND, due, due + MICROSECOND] {
        assert_evaluation(
            &rules,
            &facts,
            now,
            Some(PlannedAction::SetStorageClass(StorageClass::Archive)),
            None,
        );
    }
}

#[test]
fn next_due_skips_boundaries_that_cannot_change_the_selected_action() {
    let facts = version();
    let rules = rules(&[
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":1}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":2}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":3}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"ARCHIVE"},"condition":{"age":4}}),
        json!({"action":{"type":"Delete"},"condition":{"age":5}}),
        json!({"action":{"type":"Delete"},"condition":{"age":6}}),
        json!({"action":{"type":"Delete"},"condition":{"age":0,"isLive":false}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"STANDARD"},"condition":{"age":0}}),
    ]);
    let selected = [
        (
            1,
            PlannedAction::SetStorageClass(StorageClass::Nearline),
            Some(3),
        ),
        (
            3,
            PlannedAction::SetStorageClass(StorageClass::Coldline),
            Some(4),
        ),
        (
            4,
            PlannedAction::SetStorageClass(StorageClass::Archive),
            Some(5),
        ),
        (5, PlannedAction::Delete, None),
    ];
    let mut previous = None;
    for (day, action, next) in selected {
        let due = facts.time_created + day * DAY;
        assert_evaluation(&rules, &facts, due - MICROSECOND, previous, Some(due));
        for now in [due, due + MICROSECOND] {
            assert_evaluation(
                &rules,
                &facts,
                now,
                Some(action),
                next.map(|day| facts.time_created + day * DAY),
            );
        }
        previous = Some(action);
    }
    for day in [2, 6] {
        let due = facts.time_created + day * DAY;
        let (action, next) = if day == 2 {
            (
                PlannedAction::SetStorageClass(StorageClass::Nearline),
                Some(facts.time_created + 3 * DAY),
            )
        } else {
            (PlannedAction::Delete, None)
        };
        for now in [due - MICROSECOND, due, due + MICROSECOND] {
            assert_evaluation(&rules, &facts, now, Some(action), next);
        }
    }
}

#[test]
fn next_due_uses_the_latest_condition_and_earliest_matching_rule() {
    let mut facts = version();
    facts.custom_time = Some(facts.time_created + DAY);
    facts.noncurrent_time = Some(facts.time_created + 2 * DAY);
    let rules = [
        delete(json!({"age":1,"daysSinceCustomTime":2,"daysSinceNoncurrentTime":2})),
        delete(json!({"age":5})),
    ];
    assert_delete_boundary(&rules, &facts, facts.time_created + 4 * DAY);
    for day in [1, 3] {
        let earlier = facts.time_created + day * DAY;
        for now in [earlier - MICROSECOND, earlier, earlier + MICROSECOND] {
            assert_evaluation(
                &rules,
                &facts,
                now,
                None,
                Some(facts.time_created + 4 * DAY),
            );
        }
    }
}

#[test]
fn delete_only_defers_class_dependent_deletes_when_a_transition_rule_exists() {
    let facts = version();
    let rules = rules(&[
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":30}}),
        json!({"action":{"type":"Delete"},"condition":{"age":60,"matchesStorageClass":["STANDARD"]}}),
    ]);
    for now in [
        facts.time_created,
        facts.time_created + 30 * DAY,
        facts.time_created + 60 * DAY - MICROSECOND,
        facts.time_created + 60 * DAY,
        facts.time_created + 60 * DAY + MICROSECOND,
    ] {
        let result = evaluate_delete_only(&rules, &facts, now);
        assert_eq!(result.action, None);
        assert_eq!(result.next_due, None);
    }
}

#[test]
fn delete_only_keeps_class_conditions_without_transition_rules() {
    let facts = version();
    let rules = [delete(json!({"age":60,"matchesStorageClass":["STANDARD"]}))];
    let due = facts.time_created + 60 * DAY;
    for now in [due - MICROSECOND, due, due + MICROSECOND] {
        let result = evaluate_delete_only(&rules, &facts, now);
        let expected = evaluate(&rules, &facts, now);
        assert_eq!(result.action, expected.action);
        assert_eq!(result.next_due, expected.next_due);
    }
    assert_eq!(
        evaluate_delete_only(&rules, &facts, due).action,
        Some(PlannedAction::Delete)
    );
}

#[test]
fn delete_only_ignores_class_transitions_but_keeps_independent_deletes() {
    let facts = version();
    let rules = rules(&[
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":30}}),
        json!({"action":{"type":"Delete"},"condition":{"age":60}}),
    ]);
    let due = facts.time_created + 60 * DAY;
    for now in [
        facts.time_created,
        facts.time_created + 30 * DAY,
        due - MICROSECOND,
    ] {
        let result = evaluate_delete_only(&rules, &facts, now);
        assert_eq!(result.action, None);
        assert_eq!(result.next_due, Some(due));
    }
    for now in [due, due + MICROSECOND] {
        let result = evaluate_delete_only(&rules, &facts, now);
        assert_eq!(result.action, Some(PlannedAction::Delete));
        assert_eq!(result.next_due, None);
    }
}
