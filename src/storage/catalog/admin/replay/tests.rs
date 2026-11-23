use super::*;

/// The version seeding published, one an API write of a body made after it, and the version an
/// API edit of the metadata alone made of the one seeding published.
const SEEDED: Version = Version {
    generation: 7,
    metageneration: 1,
};
const API: Version = Version {
    generation: 9,
    metageneration: 1,
};
const METADATA_EDITED: Version = Version {
    metageneration: 2,
    ..SEEDED
};

/// When the fixture file last changed.
fn written() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn identity() -> FileIdentity {
    let nanos = crate::clock::signed_nanos(written());
    FileIdentity {
        device: 1,
        inode: 42,
        size: 5,
        modified_ns: nanos,
        changed_ns: nanos,
    }
}

/// A fingerprint of `identity` taken long after the file last changed, so it is not racy.
fn settled(identity: FileIdentity) -> Fingerprint {
    Fingerprint::new(identity, written() + Duration::from_secs(60))
}

fn body(byte: u8) -> Body {
    Body {
        size: 5,
        hashes: Hashes {
            md5: [byte; 16],
            crc32c: u32::from(byte),
        },
    }
}

/// What the fixture file held when seeding recorded it, and what it holds after an edit.
fn recorded() -> Body {
    body(1)
}

fn edited() -> Body {
    body(2)
}

/// A record of the file `recorded()` that owns `owned`, taken from a settled stat.
fn record(owned: Option<Version>) -> SeedRecord {
    SeedRecord::new(owned, recorded(), settled(identity()))
}

fn live(version: Version, body: Body, protection: Protection) -> LiveObject {
    LiveObject {
        version,
        body,
        attributes: Attributes::AsSeeded,
        protection,
    }
}

/// Every state of a live object at a name holding `recorded()`: none, the one seeding
/// published, an API overwrite with other content, and each with and without protection.
fn every_live_object() -> [Option<LiveObject>; 5] {
    [
        None,
        Some(live(SEEDED, recorded(), Protection::Unprotected)),
        Some(live(SEEDED, recorded(), Protection::Protected)),
        Some(live(API, edited(), Protection::Unprotected)),
        Some(live(API, edited(), Protection::Protected)),
    ]
}

/// A file is published where no object or a different one is live, and fails as protected where
/// a hold or retention refuses replacing a different one. An object with the file's body and
/// only the seed attributes is recorded instead, with protection refusing nothing.
#[test]
fn a_startup_replay_publishes_a_file_unless_the_live_object_already_holds_it() {
    let file = recorded();
    let expected = [
        Replay::Publish,
        Replay::Record { owned: None },
        Replay::Record { owned: None },
        Replay::Publish,
        Replay::Protected,
    ];
    for (live, expected) in every_live_object().iter().zip(expected) {
        assert_eq!(
            startup_file(&file, None, live.as_ref()),
            expected,
            "{live:?}"
        );
    }
    for protection in [Protection::Unprotected, Protection::Protected] {
        let metadata_edit = LiveObject {
            attributes: Attributes::Edited,
            ..live(METADATA_EDITED, recorded(), protection)
        };
        let expected = match protection {
            Protection::Unprotected => Replay::Publish,
            Protection::Protected => Replay::Protected,
        };
        assert_eq!(startup_file(&file, None, Some(&metadata_edit)), expected);
    }
}

/// Seeding owns the object it records only when its record already owned exactly that live
/// version. An identical object that an API write made, one whose metadata an API write edited
/// and then restored, or one that seeding wrote before it kept records, is recorded unowned, so
/// that removing the file later never deletes it.
#[test]
fn recording_an_identical_live_object_keeps_ownership_only_of_the_version_seeding_owned() {
    let file = recorded();
    let seeded = live(SEEDED, recorded(), Protection::Unprotected);
    let api_written = live(API, recorded(), Protection::Unprotected);
    let restored_metadata = live(METADATA_EDITED, recorded(), Protection::Unprotected);
    let cases = [
        (record(Some(SEEDED)), seeded, Some(SEEDED)),
        (record(Some(SEEDED)), api_written, None),
        (record(Some(SEEDED)), restored_metadata, None),
        (record(None), seeded, None),
        (record(Some(API)), seeded, None),
    ];
    for (record, live, owned) in cases {
        assert_eq!(
            startup_file(&file, Some(&record), Some(&live)),
            Replay::Record { owned },
            "{record:?} {live:?}"
        );
    }
    assert_eq!(
        startup_file(&file, None, Some(&api_written)),
        Replay::Record { owned: None }
    );
}

/// What `chmod -R` or an extended attribute change (the status change time), `touch` (both
/// times), a remount (the device) and an inode number that is not stable across mounts do to a
/// stat of a file whose content they leave as it was.
fn attribute_changes() -> [FileIdentity; 4] {
    let recorded = identity();
    [
        FileIdentity {
            changed_ns: recorded.changed_ns + 1,
            ..recorded
        },
        FileIdentity {
            modified_ns: recorded.modified_ns + 1,
            changed_ns: recorded.changed_ns + 1,
            ..recorded
        },
        FileIdentity {
            device: 2,
            ..recorded
        },
        FileIdentity {
            inode: 43,
            ..recorded
        },
    ]
}

/// A pass while running reads a file again when its fingerprint differs from the record's, but
/// a file whose hashes still equal the record's only refreshes the record, whatever is live:
/// the content decides, so a changed attribute never replays the file over an API delete or
/// overwrite.
#[test]
fn a_changed_fingerprint_reads_the_file_and_unchanged_content_keeps_everything() {
    let record = record(Some(SEEDED));
    for changed in attribute_changes() {
        assert_eq!(
            live_file(Some(&record), &settled(changed)),
            Look::Read,
            "{changed:?}"
        );
        for live in every_live_object() {
            assert_eq!(
                live_read(&recorded(), Some(&record), live.as_ref()),
                Reread::Refresh,
                "{live:?}"
            );
        }
    }
    assert_eq!(
        live_file(Some(&record), &settled(identity())),
        Look::Unchanged
    );
}

/// A file whose content changed while the server runs is replayed as a startup replay would
/// replay it, and so is any file at a name without a record.
#[test]
fn a_changed_file_or_one_without_a_record_is_replayed_as_at_startup() {
    let record = record(Some(SEEDED));
    assert_eq!(
        live_file(
            None,
            &settled(FileIdentity {
                size: 6,
                ..identity()
            })
        ),
        Look::Read
    );
    for live in every_live_object() {
        assert_eq!(
            live_read(&edited(), Some(&record), live.as_ref()),
            Reread::Changed(startup_file(&edited(), Some(&record), live.as_ref())),
            "{live:?}"
        );
        assert_eq!(
            live_read(&recorded(), None, live.as_ref()),
            Reread::Changed(startup_file(&recorded(), None, live.as_ref())),
            "{live:?}"
        );
    }
}

/// A record whose stat fell within two seconds after the file's last modification or status
/// change, or before it, is racy: the next pass reads the file although the fingerprint is
/// equal. Unchanged content then keeps everything, even where nothing or an API overwrite is
/// live. A stat taken later makes a record that is not racy.
#[test]
fn a_racy_record_reads_an_equal_fingerprint_and_unchanged_content_still_keeps_everything() {
    let copied_with_its_mtime = FileIdentity {
        modified_ns: crate::clock::signed_nanos(written() - Duration::from_secs(3600)),
        ..identity()
    };
    let racy = [
        Fingerprint::new(identity(), written() + Duration::from_millis(1_999)),
        Fingerprint::new(identity(), written() - Duration::from_secs(1)),
        Fingerprint::new(copied_with_its_mtime, written() + Duration::from_secs(1)),
    ];
    for fingerprint in racy {
        let record = SeedRecord {
            fingerprint,
            ..record(Some(SEEDED))
        };
        assert_eq!(
            live_file(Some(&record), &fingerprint),
            Look::Read,
            "{fingerprint:?}"
        );
        for live in [
            None,
            Some(live(API, edited(), Protection::Unprotected)),
            Some(live(API, edited(), Protection::Protected)),
        ] {
            assert_eq!(
                live_read(&recorded(), Some(&record), live.as_ref()),
                Reread::Refresh
            );
        }
    }
    let later = Fingerprint::new(identity(), written() + Duration::from_secs(2));
    let record = SeedRecord {
        fingerprint: later,
        ..record(Some(SEEDED))
    };
    assert_eq!(live_file(Some(&record), &later), Look::Unchanged);
}

/// A change refused by protection while the server runs is recorded with the generation seeding
/// owned before. The next pass finds the record matching the file, so the old change does not
/// win once the hold is released: the object stays until the file changes again.
#[test]
fn a_change_protection_refused_while_running_is_not_replayed_after_the_hold_is_released() {
    let before = record(Some(SEEDED));
    let held = live(SEEDED, recorded(), Protection::Protected);
    assert_eq!(
        live_read(&edited(), Some(&before), Some(&held)),
        Reread::Changed(Replay::Protected)
    );
    let changed = FileIdentity {
        size: 6,
        ..identity()
    };
    let after = SeedRecord {
        body: edited(),
        fingerprint: settled(changed),
        ..before
    };
    let released = live(SEEDED, recorded(), Protection::Unprotected);
    assert_eq!(live_file(Some(&after), &settled(changed)), Look::Unchanged);
    assert_eq!(
        live_read(&edited(), Some(&after), Some(&released)),
        Reread::Refresh
    );
}

/// A proof that a name holds nothing, made by the lookup that is the only source of one.
#[cfg(unix)]
fn vacant() -> Vacant {
    let directory = crate::test_support::TestDir::new();
    let parent = std::fs::File::open(&directory.0).unwrap();
    match crate::fsutil::Lookup::unprobed().regular_unconfirmed(&parent, "missing".as_ref()) {
        Ok(crate::fsutil::Opened::Missing(vacant)) => vacant,
        Ok(_) | Err(_) => panic!("a name that holds nothing is proven vacant"),
    }
}

/// What an API edit of the metadata alone makes of the object seeding published.
#[cfg(unix)]
fn metadata_edit() -> LiveObject {
    LiveObject {
        attributes: Attributes::Edited,
        ..live(METADATA_EDITED, recorded(), Protection::Unprotected)
    }
}

/// A name proven absent deletes the live object only while it is the version seeding owns and
/// still holds the body the record read, conditioned on that generation and metageneration;
/// protection skips the deletion. Without ownership, with another generation live, after an API
/// edit of the metadata, with a changed body or with nothing live, nothing is deleted: a pass
/// while running keeps a tombstone, and a startup replay drops the record.
#[cfg(unix)]
#[test]
fn an_absent_file_deletes_only_the_unchanged_version_seeding_owns() {
    let vacant = vacant();
    let owner = record(Some(SEEDED));
    for phase in [Phase::Startup, Phase::Live] {
        assert_eq!(
            absent(
                phase,
                &vacant,
                &owner,
                Some(&live(SEEDED, recorded(), Protection::Unprotected))
            ),
            Vanished::Delete { owned: SEEDED }
        );
        assert_eq!(
            absent(
                phase,
                &vacant,
                &owner,
                Some(&live(SEEDED, recorded(), Protection::Protected))
            ),
            Vanished::Protected
        );
        let not_owned = [
            (owner, Some(live(API, recorded(), Protection::Unprotected))),
            (owner, Some(metadata_edit())),
            (owner, Some(live(SEEDED, edited(), Protection::Unprotected))),
            (owner, None),
            (
                record(None),
                Some(live(SEEDED, recorded(), Protection::Unprotected)),
            ),
        ];
        let expected = match phase {
            Phase::Startup => Vanished::Forget,
            Phase::Live => Vanished::Tombstone,
        };
        for (record, live) in not_owned {
            assert_eq!(
                absent(phase, &vacant, &record, live.as_ref()),
                expected,
                "{phase:?} {record:?} {live:?}"
            );
        }
    }
}

/// A recorded name the lookup could not tell about keeps reporting only while seeding purely
/// owns the live object, protected or not. Once an API write replaced or edited it, or nothing is
/// live, or seeding owns nothing, the record becomes a tombstone without a proof, as a proof would
/// leave it.
#[test]
fn an_unproven_name_is_reported_only_while_seeding_purely_owns_the_live_object() {
    let owner = record(Some(SEEDED));
    for protection in [Protection::Unprotected, Protection::Protected] {
        assert_eq!(
            unproven(&owner, Some(&live(SEEDED, recorded(), protection))),
            Unproven::Report
        );
    }
    let edited_metadata = LiveObject {
        attributes: Attributes::Edited,
        ..live(METADATA_EDITED, recorded(), Protection::Unprotected)
    };
    let not_owned = [
        (owner, Some(live(API, recorded(), Protection::Unprotected))),
        (owner, Some(edited_metadata)),
        (owner, Some(live(SEEDED, edited(), Protection::Unprotected))),
        (owner, None),
        (
            record(None),
            Some(live(SEEDED, recorded(), Protection::Unprotected)),
        ),
        (owner.after_deleting(SEEDED), None),
    ];
    for (record, live) in not_owned {
        assert_eq!(
            unproven(&record, live.as_ref()),
            Unproven::Tombstone,
            "{record:?} {live:?}"
        );
    }
}

/// An editor's rename-save unlinks the fixture file for a moment and renames a new one with the
/// same content into its place. Where an API write replaced the object seeding published, the
/// pass that finds the name empty keeps a tombstone, and the next one, finding the content the
/// record read, only refreshes the record: the API write stays.
#[cfg(unix)]
#[test]
fn a_file_back_with_its_body_after_a_rename_save_keeps_an_api_write() {
    let api = live(API, edited(), Protection::Unprotected);
    let owner = record(Some(SEEDED));
    assert_eq!(
        absent(Phase::Live, &vacant(), &owner, Some(&api)),
        Vanished::Tombstone
    );
    let tombstone = owner.tombstone();
    let renamed = settled(FileIdentity {
        inode: 43,
        ..identity()
    });
    assert_eq!(live_file(Some(&tombstone), &renamed), Look::Read);
    assert_eq!(
        live_read(&recorded(), Some(&tombstone), Some(&api)),
        Reread::Refresh
    );
}

/// `git checkout other && git checkout -` removes the fixture file for as many passes as the
/// other branch stays checked out, then writes the same content back. Each pass in between keeps
/// the tombstone, so an API write of a body or of the metadata stays, protected or not.
#[cfg(unix)]
#[test]
fn a_file_back_with_its_body_after_a_branch_switch_keeps_an_api_write() {
    let vacant = vacant();
    let restored = settled(FileIdentity {
        modified_ns: identity().modified_ns + 5_000_000_000,
        changed_ns: identity().changed_ns + 5_000_000_000,
        ..identity()
    });
    for api in [
        live(API, edited(), Protection::Unprotected),
        live(API, edited(), Protection::Protected),
        metadata_edit(),
    ] {
        let mut kept = record(Some(SEEDED));
        for _ in 0..3 {
            assert_eq!(
                absent(Phase::Live, &vacant, &kept, Some(&api)),
                Vanished::Tombstone,
                "{api:?}"
            );
            kept = kept.tombstone();
        }
        assert_eq!(live_file(Some(&kept), &restored), Look::Read);
        assert_eq!(
            live_read(&recorded(), Some(&kept), Some(&api)),
            Reread::Refresh,
            "{api:?}"
        );
    }
}

/// A file that returns to a tombstone with other content is a changed file, replayed over what
/// is live as any changed file is.
#[cfg(unix)]
#[test]
fn a_file_back_with_other_content_is_replayed_over_an_api_write() {
    let api = live(API, edited(), Protection::Unprotected);
    let owner = record(Some(SEEDED));
    assert_eq!(
        absent(Phase::Live, &vacant(), &owner, Some(&api)),
        Vanished::Tombstone
    );
    assert_eq!(
        live_read(&body(3), Some(&owner.tombstone()), Some(&api)),
        Reread::Changed(Replay::Publish)
    );
}

/// The record seeding keeps once it deleted its own object for a file proven gone, in either
/// phase.
#[cfg(unix)]
fn deleted_by_seeding() -> SeedRecord {
    let seeded = live(SEEDED, recorded(), Protection::Unprotected);
    let owner = record(Some(SEEDED));
    for phase in [Phase::Startup, Phase::Live] {
        assert_eq!(
            absent(phase, &vacant(), &owner, Some(&seeded)),
            Vanished::Delete { owned: SEEDED }
        );
    }
    let deleted = owner.after_deleting(SEEDED);
    assert_eq!((deleted.owned, deleted.deleted), (None, Some(SEEDED)));
    // A later pass that still finds the name empty owns nothing there to delete.
    assert_eq!(
        absent(Phase::Live, &vacant(), &deleted, None),
        Vanished::Tombstone
    );
    deleted
}

/// A file that returns with the body of the object seeding deleted for it is published again
/// where nothing is live, even with the very fingerprint the record keeps, as when its directory
/// was renamed away and back.
#[cfg(unix)]
#[test]
fn a_file_back_after_seeding_deleted_its_object_is_published_again() {
    let deleted = deleted_by_seeding();
    assert_eq!(live_file(Some(&deleted), &settled(identity())), Look::Read);
    assert_eq!(
        live_read(&recorded(), Some(&deleted), None),
        Reread::Changed(Replay::Publish)
    );
}

/// `git checkout other` long enough for seeding to delete its object, an API write of the name,
/// then `git checkout -`: the file returns with the body seeding deleted, and the API write stays,
/// protected or not, of a body or of the metadata. The refreshed record forgets the deletion, so
/// a later `touch` with nothing live, once the API deleted its object too, publishes nothing.
#[cfg(unix)]
#[test]
fn a_file_back_after_seeding_deleted_its_object_keeps_an_api_write_made_since() {
    let deleted = deleted_by_seeding();
    for api in [
        live(API, edited(), Protection::Unprotected),
        live(API, edited(), Protection::Protected),
        live(API, recorded(), Protection::Unprotected),
        metadata_edit(),
    ] {
        assert_eq!(
            live_read(&recorded(), Some(&deleted), Some(&api)),
            Reread::Refresh,
            "{api:?}"
        );
    }
    let touched = settled(FileIdentity {
        modified_ns: identity().modified_ns + 1,
        ..identity()
    });
    let refreshed = deleted.refreshed(touched);
    assert_eq!(refreshed.deleted, None);
    assert_eq!(live_file(Some(&refreshed), &touched), Look::Unchanged);
    assert_eq!(
        live_read(&recorded(), Some(&refreshed), None),
        Reread::Refresh
    );
}

/// A file that returns with other content after seeding deleted its object is a changed file,
/// replayed over whatever is live as any changed file is.
#[cfg(unix)]
#[test]
fn a_file_back_with_other_content_after_seeding_deleted_its_object_is_replayed() {
    let deleted = deleted_by_seeding();
    assert_eq!(
        live_read(&edited(), Some(&deleted), None),
        Reread::Changed(Replay::Publish)
    );
    let api = live(API, recorded(), Protection::Unprotected);
    assert_eq!(
        live_read(&edited(), Some(&deleted), Some(&api)),
        Reread::Changed(Replay::Publish)
    );
}

/// While the server runs, a deletion waits until a second proof comes at least a second after
/// the first. A proof dated after the pass, by a clock that moved back, starts the wait again.
#[test]
fn a_deletion_while_running_waits_for_a_second_proof_a_second_later() {
    let first = written();
    let owner = record(Some(SEEDED));
    let proof = Absence {
        since: first,
        pass: 4,
    };
    assert_eq!(
        confirm_absence(owner.absent, 4, first),
        Confirmation::Pending(proof)
    );
    let early = first + Duration::from_millis(999);
    assert_eq!(
        confirm_absence(Some(proof), 5, early),
        Confirmation::Pending(Absence {
            since: first,
            pass: 5
        })
    );
    assert_eq!(
        confirm_absence(Some(proof), 5, first + Duration::from_secs(1)),
        Confirmation::Confirmed
    );
    let before = first - Duration::from_secs(10);
    assert_eq!(
        confirm_absence(Some(proof), 5, before),
        Confirmation::Pending(Absence {
            since: before,
            pass: 5
        })
    );
}

/// A pass between two proofs of absence that finds an entry under the name, here one that keeps
/// failing to open, ends the wait: the proofs count only from consecutive passes, so the next
/// proof starts the wait again rather than confirming the deletion.
#[test]
fn an_entry_found_between_two_proofs_of_absence_starts_the_wait_again() {
    let first = written();
    let proven = SeedRecord {
        absent: Some(Absence {
            since: first,
            pass: 1,
        }),
        ..record(Some(SEEDED))
    };
    assert_eq!(
        unread(Phase::Live, "unopenable"),
        Unread::Report("unopenable")
    );
    let found = proven.found();
    let third = first + Duration::from_secs(2);
    assert_eq!(
        confirm_absence(found.absent, 3, third),
        Confirmation::Pending(Absence {
            since: third,
            pass: 3
        })
    );
    assert_eq!(
        confirm_absence(proven.absent, 2, third),
        Confirmation::Confirmed
    );
}

/// A pass that failed before it committed, between two proofs of absence, leaves a run whose
/// last pass is not the one before: the later proof starts the wait again, however long ago the
/// first was.
#[test]
fn a_failed_pass_between_two_proofs_of_absence_starts_the_wait_again() {
    let first = written();
    let proof = Some(Absence {
        since: first,
        pass: 1,
    });
    let third = first + Duration::from_secs(5);
    assert_eq!(
        confirm_absence(proof, 3, third),
        Confirmation::Pending(Absence {
            since: third,
            pass: 3
        })
    );
    assert_eq!(confirm_absence(proof, 2, third), Confirmation::Confirmed);
}

/// A name that could not be read fails a startup replay with its reason, and is reported with
/// its reason while the server runs, never taken for an unchanged one.
#[test]
fn an_unread_name_fails_startup_and_is_reported_while_running() {
    assert_eq!(unread(Phase::Startup, "reason"), Unread::Fail("reason"));
    assert_eq!(unread(Phase::Live, "reason"), Unread::Report("reason"));
}
