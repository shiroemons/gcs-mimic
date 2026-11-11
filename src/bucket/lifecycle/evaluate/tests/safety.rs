use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    class: Option<StorageClass>,
    visited: u8,
    yielded: bool,
}

impl State {
    fn new() -> Self {
        Self {
            class: Some(StorageClass::Standard),
            visited: bit(StorageClass::Standard),
            yielded: false,
        }
    }

    fn pass(&mut self, mut select: impl FnMut(StorageClass) -> (Option<PlannedAction>, bool)) {
        while let Some(class) = self.class {
            let (action, yielded) = select(class);
            self.yielded |= yielded;
            match action {
                None => return,
                Some(PlannedAction::Delete) => self.class = None,
                Some(PlannedAction::SetStorageClass(next)) => {
                    assert_eq!(
                        self.visited & bit(next),
                        0,
                        "a pass must not revisit a class"
                    );
                    self.visited |= bit(next);
                    self.class = Some(next);
                }
            }
        }
    }
}

fn bit(class: StorageClass) -> u8 {
    1 << match class {
        StorageClass::DurableReducedAvailability => 0,
        StorageClass::Standard => 1,
        StorageClass::MultiRegional => 2,
        StorageClass::Nearline => 3,
        StorageClass::Coldline => 4,
        StorageClass::Archive => 5,
    }
}

struct OracleRule<'a> {
    rule: &'a LifecycleRule,
    due: Option<SystemTime>,
}

impl<'a> OracleRule<'a> {
    fn new(rule: &'a LifecycleRule, facts: &VersionFacts<'_>) -> Self {
        let condition = &rule.condition;
        // The generated domain has no name/size conditions; boundary tests cover those separately.
        assert!(condition.matches_prefix.is_empty() && condition.matches_suffix.is_empty());
        assert!(condition.size_above_bytes.is_none() && condition.size_below_bytes.is_none());
        let present = condition
            .is_live
            .is_none_or(|live| live == facts.noncurrent_time.is_none())
            && condition
                .num_newer_versions
                .is_none_or(|count| facts.newer_versions >= count as u64)
            && [
                (condition.created_before, Some(facts.time_created)),
                (condition.custom_time_before, facts.custom_time),
                (condition.noncurrent_time_before, facts.noncurrent_time),
            ]
            .into_iter()
            .all(|(date, origin)| {
                date.is_none_or(|date| {
                    let midnight = time(&format!(
                        "{:04}-{:02}-{:02}T00:00:00Z",
                        date.year, date.month, date.day
                    ));
                    origin.is_some_and(|origin| origin < midnight)
                })
            });
        let mut due = present.then_some(facts.time_created);
        for (days, origin) in [
            (condition.age, Some(facts.time_created)),
            (condition.days_since_custom_time, facts.custom_time),
            (condition.days_since_noncurrent_time, facts.noncurrent_time),
        ] {
            if let Some(days) = days {
                due = due
                    .zip(origin)
                    .map(|(due, origin)| due.max(origin + DAY * days as u32));
            }
        }
        if condition.age == Some(0) {
            // Every generated creation time is UTC midnight, so age zero first matches one day later.
            due = due.map(|due| due.max(facts.time_created + DAY));
        }
        Self { rule, due }
    }

    fn candidate(&self, class: StorageClass, now: SystemTime) -> Option<PlannedAction> {
        if self.due.is_none_or(|due| due > now)
            || (!self.rule.condition.matches_storage_class.is_empty()
                && !self
                    .rule
                    .condition
                    .matches_storage_class
                    .contains(&LifecycleMatchClass::Supported(class)))
        {
            return None;
        }
        // This independent priority oracle never calls evaluate, eligible_at, or planned_action.
        match self.rule.action {
            LifecycleAction::Delete => Some(PlannedAction::Delete),
            LifecycleAction::SetStorageClass(target) => {
                let supported = matches!(
                    (class, target),
                    (
                        StorageClass::Standard,
                        StorageClass::Nearline | StorageClass::Coldline | StorageClass::Archive
                    ) | (
                        StorageClass::Nearline,
                        StorageClass::Coldline | StorageClass::Archive
                    ) | (StorageClass::Coldline, StorageClass::Archive)
                );
                supported.then_some(PlannedAction::SetStorageClass(target))
            }
            LifecycleAction::AbortIncompleteMultipartUpload => {
                unreachable!("not in the rule domain")
            }
        }
    }
}

fn baseline(
    rules: &[OracleRule<'_>],
    class: StorageClass,
    now: SystemTime,
) -> Option<PlannedAction> {
    rules
        .iter()
        .filter_map(|rule| rule.candidate(class, now))
        .min_by_key(|action| match action {
            PlannedAction::Delete => 0,
            PlannedAction::SetStorageClass(class) => 64 - bit(*class),
        })
}

fn actual(
    rules: &[LifecycleRule],
    facts: &mut VersionFacts<'_>,
    class: StorageClass,
    now: SystemTime,
) -> Option<PlannedAction> {
    facts.storage_class = class;
    match evaluate(rules, facts, now) {
        Evaluation::Act(action) => Some(action),
        Evaluation::Wait(_) => None,
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Counts {
    unbranched: usize,
    branched: usize,
    delayed_deletion_differences: usize,
    timely_differences: usize,
}

fn compare(configured: &[LifecycleRule], totals: &mut Counts) {
    let unbranched = configured.iter().all(|rule| match rule.action {
        LifecycleAction::SetStorageClass(_) => rule.condition.matches_storage_class.is_empty(),
        LifecycleAction::Delete | LifecycleAction::AbortIncompleteMultipartUpload => true,
    });
    // Six fixed fact snapshots cover live/noncurrent, absent/current/past Custom-Time, and 0/1/2 successors.
    // Delete is terminal here; versioned archiving and changing successor evidence have catalog tests.
    for (custom_days, noncurrent, newer) in [
        (None, false, 0),
        (Some(0), false, 0),
        (Some(5), false, 0),
        (None, true, 0),
        (Some(0), true, 1),
        (Some(5), true, 2),
    ] {
        let mut facts = version();
        facts.time_created = time("2026-01-10T00:00:00Z");
        facts.custom_time = custom_days.map(|days| facts.time_created - DAY * days);
        facts.noncurrent_time = noncurrent.then_some(facts.time_created);
        facts.newer_versions = newer;
        let oracle: Vec<_> = configured
            .iter()
            .map(|rule| OracleRule::new(rule, &facts))
            .collect();
        let mut scheduled = State::new();
        let mut timely = State::new();
        for day in 0..=40 {
            let now = facts.time_created + DAY * day;
            scheduled.pass(|class| (baseline(&oracle, class, now), false));
            timely.pass(|class| {
                let selected = actual(configured, &mut facts, class, now);
                let original = baseline(&oracle, class, now);
                let differs = selected != original;
                if differs {
                    assert_eq!(original, Some(PlannedAction::Delete));
                    assert!(matches!(selected, Some(PlannedAction::SetStorageClass(_))));
                    assert_eq!(
                        selected,
                        oracle
                            .iter()
                            .filter_map(|rule| match rule.candidate(class, now) {
                                Some(PlannedAction::SetStorageClass(target)) => Some(target),
                                Some(PlannedAction::Delete) | None => None,
                            })
                            .max_by_key(|target| bit(*target))
                            .map(PlannedAction::SetStorageClass),
                        "yielding must choose the oracle's coldest applicable transition"
                    );
                    assert!(
                        oracle
                            .iter()
                            .any(|rule| rule.candidate(class, now) == Some(PlannedAction::Delete))
                    );
                    assert!(!oracle.iter().any(|rule| rule.candidate(class, now)
                        == Some(PlannedAction::Delete)
                        && rule.rule.condition.matches_storage_class.is_empty()));
                }
                (selected, differs)
            });
            if ![1, 10, 20, 30, 40].contains(&day) {
                continue;
            }
            let mut delayed = State::new();
            delayed.pass(|class| (actual(configured, &mut facts, class, now), false));
            if unbranched {
                totals.unbranched += 1;
            } else {
                totals.branched += 1;
            }
            if delayed.class.is_none() && scheduled.class.is_some() {
                assert!(
                    !unbranched,
                    "ungated rules={} T={day} facts=({custom_days:?},{noncurrent},{newer})",
                    serde_json::to_string(configured).unwrap()
                );
                assert_ne!(
                    delayed.visited & !scheduled.visited,
                    0,
                    "non-branching mismatch: rules={} T={day}",
                    serde_json::to_string(configured).unwrap()
                );
                totals.delayed_deletion_differences += 1;
            }
            if timely.class != scheduled.class {
                assert!(
                    scheduled.class.is_none() && timely.class.is_some(),
                    "a timely difference must retain the object that the oracle deleted"
                );
                assert!(
                    timely.yielded,
                    "difference without a simultaneous class-dependent Delete and transition"
                );
                totals.timely_differences += 1;
            }
        }
    }
}

fn cases() -> Vec<LifecycleRule> {
    // Exhaust all pairs over atoms and temporal/class conjunctions; avoid a redundant full Cartesian product.
    let mut conditions = vec![
        json!({"age":0}),
        json!({"age":10}),
        json!({"age":30}),
        json!({"isLive":true}),
        json!({"isLive":false}),
        json!({"createdBefore":"2026-01-11"}),
        json!({"customTimeBefore":"2026-01-11"}),
        json!({"noncurrentTimeBefore":"2026-01-11"}),
        json!({"numNewerVersions":2}),
    ];
    for class in ["STANDARD", "NEARLINE", "COLDLINE"] {
        conditions.push(json!({"matchesStorageClass":[class]}));
    }
    for field in [
        "daysSinceNoncurrentTime",
        "numNewerVersions",
        "daysSinceCustomTime",
    ] {
        let value = if field == "numNewerVersions" { 1 } else { 10 };
        conditions.push(json!({field:value}));
        for class in ["STANDARD", "NEARLINE"] {
            conditions.push(json!({field:value,"matchesStorageClass":[class]}));
        }
    }
    for age in [10, 30] {
        for live in [false, true] {
            conditions.push(json!({"age":age,"isLive":live}));
        }
        for class in ["STANDARD", "NEARLINE"] {
            conditions.push(json!({"age":age,"matchesStorageClass":[class]}));
        }
    }
    for live in [false, true] {
        conditions.push(json!({"isLive":live,"matchesStorageClass":["NEARLINE"]}));
    }
    let mut configured = Vec::new();
    for action in [
        LifecycleAction::Delete,
        LifecycleAction::SetStorageClass(StorageClass::Nearline),
        LifecycleAction::SetStorageClass(StorageClass::Coldline),
        LifecycleAction::SetStorageClass(StorageClass::Archive),
    ] {
        for condition in &conditions {
            configured.push(rules(&[json!({"action":action,"condition":condition})]).remove(0));
        }
    }
    configured
}

#[test]
fn delayed_deletion_safety_has_an_explicit_class_independent_transition_domain() {
    let configured = cases();
    let mut pairs = Counts::default();
    for first in 0..configured.len() {
        for second in first..configured.len() {
            compare(
                &[configured[first].clone(), configured[second].clone()],
                &mut pairs,
            );
        }
    }
    // Triples add route divergence and Delete interactions; every condition atom already appears in all pairs.
    let triples = rules(&[
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":10}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":10,"matchesStorageClass":["STANDARD"]}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":20,"matchesStorageClass":["STANDARD"]}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":30,"matchesStorageClass":["NEARLINE"]}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"ARCHIVE"},"condition":{"matchesStorageClass":["COLDLINE"]}}),
        json!({"action":{"type":"Delete"},"condition":{"age":30,"matchesStorageClass":["COLDLINE"]}}),
        json!({"action":{"type":"Delete"},"condition":{"age":30,"matchesStorageClass":["STANDARD"]}}),
        json!({"action":{"type":"Delete"},"condition":{"daysSinceCustomTime":10,"matchesStorageClass":["NEARLINE"]}}),
        json!({"action":{"type":"Delete"},"condition":{"isLive":true,"age":10}}),
        json!({"action":{"type":"Delete"},"condition":{"daysSinceNoncurrentTime":10}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"daysSinceNoncurrentTime":10}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"numNewerVersions":2}}),
    ]);
    let mut three = Counts::default();
    for first in 0..triples.len() {
        for second in first..triples.len() {
            for third in second..triples.len() {
                compare(
                    &[
                        triples[first].clone(),
                        triples[second].clone(),
                        triples[third].clone(),
                    ],
                    &mut three,
                );
            }
        }
    }
    assert_eq!(configured.len(), 124);
    assert_eq!(
        pairs,
        Counts {
            unbranched: 94_800,
            branched: 137_700,
            delayed_deletion_differences: 0,
            timely_differences: 1515
        }
    );
    assert_eq!(
        three,
        Counts {
            unbranched: 3600,
            branched: 7320,
            delayed_deletion_differences: 32,
            timely_differences: 0
        }
    );
}
