use super::*;
use crate::clock::{Clock, ManualClock};
use std::time::Duration;

/// The generation seeding wrote, and one an API write made after it.
const SEEDED: i64 = 7;
const API: i64 = 9;

/// When the fixture file last changed.
fn written() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn fingerprint() -> Fingerprint {
    let nanos = crate::clock::signed_nanos(written());
    Fingerprint {
        size: 5,
        mtime_ns: nanos,
        ctime_ns: Some(nanos),
        inode: Some(42),
        device: Some(1),
    }
}

/// A record of `fingerprint` observed long after the file last changed, so it is not racy.
fn settled(fingerprint: Fingerprint) -> SeedRecord {
    SeedRecord::new(SEEDED, fingerprint, written() + Duration::from_secs(60))
}

fn live(generation: i64, content: Content, protection: Protection) -> Option<LiveObject> {
    Some(LiveObject {
        generation,
        content,
        protection,
    })
}

/// What a startup replay and a pass while the server runs decide, in that order.
fn both(
    observation: &Observation,
    record: Option<&SeedRecord>,
    live: Option<LiveObject>,
) -> [Outcome; 2] {
    [
        decide(Phase::Startup, observation, record, live),
        decide(Phase::Live, observation, record, live),
    ]
}

/// Every state of a live object at a name, with and without protection.
fn every_live_object() -> [Option<LiveObject>; 5] {
    [
        None,
        live(SEEDED, Content::AsSeeded, Protection::Unprotected),
        live(SEEDED, Content::AsSeeded, Protection::Protected),
        live(API, Content::Different, Protection::Unprotected),
        live(API, Content::Different, Protection::Protected),
    ]
}

/// A file is replayed in both phases when the name has no record, when its fingerprint differs
/// from the record's and when the record is racy: no live object or a different one gets the
/// file published, one already as seeded only gets the record, and a protected different one
/// fails a startup replay but is skipped while the server runs.
fn assert_replayed(observation: &Observation, record: Option<&SeedRecord>) {
    let expected = [
        [Outcome::Publish, Outcome::Publish],
        [Outcome::Record, Outcome::Record],
        [Outcome::Record, Outcome::Record],
        [Outcome::Publish, Outcome::Publish],
        [Outcome::Fail, Outcome::Skip(Skip::Protected)],
    ];
    for (live, expected) in every_live_object().into_iter().zip(expected) {
        assert_eq!(both(observation, record, live), expected, "{live:?}");
    }
}

#[test]
fn a_file_without_a_record_is_published_unless_the_live_object_already_matches() {
    assert_replayed(&Observation::File(fingerprint()), None);
}

#[test]
fn a_file_whose_fingerprint_differs_from_its_record_is_replayed() {
    // Every field is `Copy`, so each update copies the rest and leaves `recorded` whole.
    let recorded = fingerprint();
    let changes = [
        Fingerprint {
            size: 6,
            ..recorded
        },
        Fingerprint {
            mtime_ns: recorded.mtime_ns + 1,
            ..recorded
        },
        Fingerprint {
            ctime_ns: recorded.ctime_ns.map(|ctime_ns| ctime_ns + 1),
            ..recorded
        },
        Fingerprint {
            inode: Some(43),
            ..recorded
        },
    ];
    let record = settled(recorded);
    for changed in changes {
        assert_replayed(&Observation::File(changed), Some(&record));
    }
}

/// A startup replay ignores records, so an API delete or overwrite of an unchanged fixture is
/// replayed over as before; while the server runs, the API's change stays until the file
/// changes, and a device number that changed alone, as after a remount, is no change.
#[test]
fn an_unchanged_file_is_replayed_at_startup_and_left_alone_while_running() {
    let record = settled(fingerprint());
    let remounted = Fingerprint {
        device: Some(2),
        ..fingerprint()
    };
    let startup = [
        Outcome::Publish,
        Outcome::Record,
        Outcome::Record,
        Outcome::Publish,
        Outcome::Fail,
    ];
    for observed in [fingerprint(), remounted] {
        let observation = Observation::File(observed);
        for (live, startup) in every_live_object().into_iter().zip(startup) {
            assert_eq!(
                both(&observation, Some(&record), live),
                [startup, Outcome::Keep],
                "{live:?}"
            );
        }
    }
}

/// A record whose observation fell within two seconds after the file's last modification or
/// status change, or before it, is racy: the next pass reads the file again although the
/// fingerprint is equal. Observed later, the same fingerprint makes a record that is not.
#[test]
fn a_racy_record_replays_an_equal_fingerprint() {
    let clock = ManualClock::new(written() + Duration::from_millis(1_999));
    let racy = SeedRecord::new(SEEDED, fingerprint(), clock.now());
    assert_replayed(&Observation::File(fingerprint()), Some(&racy));
    clock.set(written() - Duration::from_secs(1));
    let before = SeedRecord::new(SEEDED, fingerprint(), clock.now());
    assert_replayed(&Observation::File(fingerprint()), Some(&before));
    clock.set(written() + Duration::from_secs(2));
    let settled = SeedRecord::new(SEEDED, fingerprint(), clock.now());
    let unchanged = Observation::File(fingerprint());
    for live in every_live_object() {
        assert_eq!(
            decide(Phase::Live, &unchanged, Some(&settled), live),
            Outcome::Keep
        );
    }
}

/// A copy that keeps the modification time, such as `cp -p`, changes only the status change
/// time, which makes the record racy as well.
#[test]
fn a_recent_status_change_makes_a_record_racy_whatever_the_modification_time() {
    let clock = ManualClock::new(written() + Duration::from_secs(1));
    let copied = Fingerprint {
        mtime_ns: crate::clock::signed_nanos(written() - Duration::from_secs(3600)),
        ..fingerprint()
    };
    let record = SeedRecord::new(SEEDED, copied.clone(), clock.now());
    assert_replayed(&Observation::File(copied), Some(&record));
}

/// A proof that a name holds nothing, made by the lookup that is the only source of one.
#[cfg(unix)]
fn absent() -> Observation {
    let directory = crate::test_support::TestDir::new();
    let parent = std::fs::File::open(&directory.0).unwrap();
    let lookup = crate::fsutil::Lookup::unprobed();
    match lookup.regular_unconfirmed(&parent, "missing".as_ref()) {
        Ok(crate::fsutil::Opened::Missing(vacant)) => Observation::Absent(vacant),
        Ok(_) | Err(_) => panic!("a name that holds nothing is proven vacant"),
    }
}

/// A name proven absent deletes the live object only while it is the generation seeding
/// recorded, conditioned on that generation, in both phases; protection skips the deletion
/// instead of failing a startup replay. An API write's generation, a name without a record and
/// a name without a live object only drop the record.
#[cfg(unix)]
#[test]
fn an_absent_file_deletes_only_the_live_generation_seeding_wrote() {
    let absent = absent();
    let record = settled(fingerprint());
    for content in [Content::AsSeeded, Content::Different] {
        assert_eq!(
            both(
                &absent,
                Some(&record),
                live(SEEDED, content, Protection::Unprotected)
            ),
            [Outcome::Delete { generation: SEEDED }; 2]
        );
        assert_eq!(
            both(
                &absent,
                Some(&record),
                live(SEEDED, content, Protection::Protected)
            ),
            [Outcome::Skip(Skip::Protected); 2]
        );
    }
    for protection in [Protection::Unprotected, Protection::Protected] {
        assert_eq!(
            both(
                &absent,
                Some(&record),
                live(API, Content::Different, protection)
            ),
            [Outcome::Forget; 2]
        );
    }
    assert_eq!(both(&absent, Some(&record), None), [Outcome::Forget; 2]);
    for live in every_live_object() {
        assert_eq!(both(&absent, None, live), [Outcome::Forget; 2], "{live:?}");
    }
}

/// An entry that could not be opened, listed or read steadily fails a startup replay, after
/// the lookup's retries, and changes nothing while the server runs: the object stays and the
/// record is kept, whatever the record and the live object are.
#[test]
fn an_unknown_name_fails_startup_and_keeps_everything_while_running() {
    let record = settled(fingerprint());
    for record in [None, Some(&record)] {
        for live in every_live_object() {
            assert_eq!(
                both(&Observation::Unknown, record, live),
                [Outcome::Fail, Outcome::Keep],
                "{record:?} {live:?}"
            );
        }
    }
}

#[test]
fn an_unusable_entry_fails_startup_and_is_skipped_with_its_reason_while_running() {
    let record = settled(fingerprint());
    for reason in [
        Unusable::Symlink,
        Unusable::Special,
        Unusable::NonUtf8Name,
        Unusable::InvalidName,
    ] {
        for record in [None, Some(&record)] {
            for live in every_live_object() {
                assert_eq!(
                    both(&Observation::Unusable(reason), record, live),
                    [Outcome::Fail, Outcome::Skip(Skip::Unusable(reason))],
                    "{reason:?} {record:?} {live:?}"
                );
            }
        }
    }
}
