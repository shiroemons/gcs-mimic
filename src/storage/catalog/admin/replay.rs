//! What a seed replay does at one fixture name, decided without any I/O from what a walk of the
//! seed directory found there, what seeding recorded when it last read the name's file, and what
//! the catalog holds.
//!
//! The decision keeps three promises:
//!
//! - Content, not attributes, decides whether a file changed. A fingerprint that differs from the
//!   record's, or a record that could have missed a change, only gets the file read and hashed
//!   again, and a file whose hashes equal the record's changes nothing in the catalog.
//! - Seeding owns a generation only when it published it, or when its record already owned
//!   exactly that live generation; it deletes only a generation it owns whose body is still the
//!   one it recorded, and only where a lookup proved the fixture file gone.
//! - A startup replay, which reseed shares, keeps the contract it had before any record existed:
//!   the fixture tree wins over the catalog, and what cannot be replayed fails it.
//!
//! Each kind of observation has a function of its own, so that a caller receives only outcomes
//! it can meet: [`startup_file`]; [`live_file`], then [`live_read`] once the file was hashed;
//! [`absent`], then [`confirm_absence`] while the server runs; and [`unread`].
//!
//! No record is kept yet, and no pass runs while the server does. The step that adds them keeps a
//! [`SeedRecord`] per replayed name in the transaction that publishes it, looks up the recorded
//! names a pass did not list, since a walk lists only what is there, and treats every record a
//! pass did not observe as unknown, not absent, once the walk failed or stopped at a limit.

use crate::{
    fsutil::{FileIdentity, Vacant},
    hashing::Hashes,
};
use std::time::{Duration, SystemTime};

/// Which replay decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The replay before the listener binds, and a reseed.
    Startup,
    /// A pass while the server runs.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pass that watches the seed directory while the server runs is not wired yet"
        )
    )]
    Live,
}

/// What a stat of a fixture file reported, and whether a change made just after the stat could
/// leave every field it reported as it was. Both are fixed when the stat is taken, so the two
/// cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    identity: FileIdentity,
    racy: bool,
}

impl Fingerprint {
    /// The fingerprint of a stat that reported `identity`, where `observed` is the system
    /// wall-clock time read *before* that stat. The file's timestamps come from a wall clock,
    /// so the injected clock does not tell this time. An observation taken earlier can only make
    /// a fingerprint racy that a later one would not, never the reverse, so reading the time
    /// first errs toward reading the file again.
    pub(crate) fn new(identity: FileIdentity, observed: SystemTime) -> Self {
        Self {
            identity,
            racy: !identity.settled_at(crate::clock::signed_nanos(observed)),
        }
    }

    /// What the stat reported.
    pub(crate) fn identity(&self) -> FileIdentity {
        self.identity
    }
}

/// What a fixture file holds, or an object's body: what decides whether a file changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Body {
    pub(crate) size: u64,
    pub(crate) hashes: Hashes,
}

/// What seeding recorded at a name when a replay last read its fixture file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeedRecord {
    /// The generation seeding owns at the name: one it published, or one the record already
    /// owned when a replay found it live and holding the file's body. An API write never passes
    /// ownership to seeding, even of an identical body.
    pub(crate) owned: Option<i64>,
    /// What the file held when a replay last read it.
    pub(crate) body: Body,
    /// The stat taken when the file was last read, or since found unchanged.
    pub(crate) fingerprint: Fingerprint,
    /// When a pass while the server runs first proved the file gone. A pass that finds the file
    /// clears it.
    pub(crate) absent_since: Option<SystemTime>,
}

/// What the catalog holds at a fixture name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveObject {
    /// The live generation.
    pub(crate) generation: i64,
    pub(crate) body: Body,
    pub(crate) attributes: Attributes,
    pub(crate) protection: Protection,
}

/// Whether the live object carries only what a replay writes besides the body: the seed content
/// type and no other metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attributes {
    AsSeeded,
    /// Anything else, such as an API metadata edit.
    Edited,
}

/// Whether a hold or an unmet retention period refuses the change a replay would make to the live
/// object: replacing it for a file, deleting it for a name proven absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protection {
    Unprotected,
    Protected,
}

/// What a replay that read a fixture file does at its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Replay {
    /// Publish the file as the name's new live generation, which seeding then owns.
    Publish,
    /// Write nothing, since the live object already is what the file would write, and record the
    /// file with `owned` as the generation seeding owns.
    Record { owned: Option<i64> },
    /// A hold or an unmet retention period refuses replacing the live object. A startup replay
    /// fails. A pass while running records the file's fingerprint and body but keeps the
    /// generation the record owned, so that the change it made is not replayed later: once the
    /// hold is released or the retention period ends, the object stays as it is until the file
    /// changes again.
    Protected,
}

/// What a replay does with a fixture file holding `file`, at a name where seeding recorded
/// `record` and the catalog holds `live`. A startup replay decides this for every file, whatever
/// its record says, so that an API delete or overwrite of an unchanged fixture is replayed over.
pub(crate) fn startup_file(
    file: &Body,
    record: Option<&SeedRecord>,
    live: Option<&LiveObject>,
) -> Replay {
    let Some(live) = live else {
        return Replay::Publish;
    };
    if live.body == *file && live.attributes == Attributes::AsSeeded {
        // Nothing would change, so protection refuses nothing. An identical object that an API
        // write made, or that seeding wrote before it kept records, stays unowned.
        let owned = record
            .and_then(|record| record.owned)
            .filter(|owned| *owned == live.generation);
        return Replay::Record { owned };
    }
    match live.protection {
        Protection::Unprotected => Replay::Publish,
        Protection::Protected => Replay::Protected,
    }
}

/// Whether a pass while the server runs reads a fixture file again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// The stat shows the file the record read, and the record could not have missed a change.
    Unchanged,
    /// Read and hash the file, then decide with [`live_read`].
    Read,
}

/// Whether a pass while the server runs reads the fixture file whose stat reported `observed`
/// again, at a name where seeding recorded `record`. A differing fingerprint is no change by
/// itself: `chmod -R`, `touch`, an extended attribute and an inode number that is not stable
/// across mounts all change it and leave the content as it was. A same-size edit that restores
/// the modification time is missed on a filesystem whose status change time does not move with
/// it, until a reseed replays every file again.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the pass that watches the seed directory while the server runs is not wired yet"
    )
)]
pub(crate) fn live_file(record: Option<&SeedRecord>, observed: &Fingerprint) -> Look {
    match record {
        Some(record)
            if !record.fingerprint.racy && record.fingerprint.identity == observed.identity =>
        {
            Look::Unchanged
        }
        Some(_) | None => Look::Read,
    }
}

/// What a pass while the server runs does with a fixture file it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reread {
    /// The file holds the body the record read: record the new fingerprint and keep everything
    /// else, so that an API delete or overwrite stays.
    Refresh,
    /// The name has no record, or the file's content changed: replay it as a startup replay
    /// would.
    Changed(Replay),
}

/// What a pass while the server runs does with a fixture file it read and found holding `file`,
/// at a name where seeding recorded `record` and the catalog holds `live`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the pass that watches the seed directory while the server runs is not wired yet"
    )
)]
pub(crate) fn live_read(
    file: &Body,
    record: Option<&SeedRecord>,
    live: Option<&LiveObject>,
) -> Reread {
    if record.is_some_and(|record| record.body == *file) {
        return Reread::Refresh;
    }
    Reread::Changed(startup_file(file, record, live))
}

/// What a replay does at a recorded name whose fixture file a lookup proved absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vanished {
    /// Delete the live object only while `generation` is still live, and drop the record. A pass
    /// while running deletes only once [`confirm_absence`] confirms it.
    Delete { generation: i64 },
    /// Change nothing and drop the record: seeding owns nothing that is still live at the name.
    Forget,
    /// Change nothing and keep the record: a hold or an unmet retention period refuses the
    /// deletion. Deleting is the one change seeding did not make before it kept records, so the
    /// deletion is skipped rather than failing a startup replay.
    Protected,
}

/// What a replay does at a name where seeding recorded `record` and the catalog holds `live`,
/// once a lookup proved the fixture file absent. A name without a record owns nothing, so a
/// caller only reports it. Seeding deletes only the generation it owns, and only while its body
/// is still the one the record read: an object that changed since is not the one seeding wrote.
#[cfg_attr(
    any(not(test), not(unix)),
    expect(
        dead_code,
        reason = "the seed record table that names what a replay owns is not wired yet, and only a Unix lookup makes the proof of absence the tests pass"
    )
)]
pub(crate) fn absent(_proof: &Vacant, record: &SeedRecord, live: Option<&LiveObject>) -> Vanished {
    let owned = record
        .owned
        .zip(live)
        .filter(|(owned, live)| live.generation == *owned && live.body == record.body);
    let Some((generation, live)) = owned else {
        return Vanished::Forget;
    };
    match live.protection {
        Protection::Unprotected => Vanished::Delete { generation },
        Protection::Protected => Vanished::Protected,
    }
}

/// How long a pass while the server runs waits between the two proofs of absence a deletion takes.
const ABSENCE_CONFIRMED_AFTER: Duration = Duration::from_secs(1);

/// Whether a pass while the server runs deletes now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirmation {
    /// Two passes at least `ABSENCE_CONFIRMED_AFTER` apart proved the file absent.
    Confirmed,
    /// Record `since` as the time of the first proof and delete nothing yet.
    Pending { since: SystemTime },
}

/// Whether a pass at `now` that [`absent`] told to delete does so. While the server runs, a
/// file can be missing for a moment, as when an editor or a sync tool replaces it by unlinking
/// it and renaming another into its place, so a deletion waits for a second proof at least
/// `ABSENCE_CONFIRMED_AFTER` after the first. A startup replay deletes on one proof: the file
/// went missing while the server was stopped.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the pass that watches the seed directory while the server runs is not wired yet"
    )
)]
pub(crate) fn confirm_absence(record: &SeedRecord, now: SystemTime) -> Confirmation {
    // A first proof, or one dated after `now` by a clock that moved back, starts the wait.
    let since = record
        .absent_since
        .filter(|since| *since <= now)
        .unwrap_or(now);
    if now.duration_since(since).unwrap_or_default() >= ABSENCE_CONFIRMED_AFTER {
        Confirmation::Confirmed
    } else {
        Confirmation::Pending { since }
    }
}

/// What a replay does at a name it could not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unread<R> {
    /// Fail the whole replay with the reason, which then changes nothing.
    Fail(R),
    /// Change nothing at the name, keep its record, and report the reason, so that a name that
    /// could not be read never passes for one found unchanged.
    Report(R),
}

/// What a `phase` replay does at a name it could not read for `reason`: an entry that could not
/// be inspected, listed or opened, one that changed while it was read, or one seeding cannot use.
/// A startup replay fails; a pass while running reports it.
pub(crate) fn unread<R>(phase: Phase, reason: R) -> Unread<R> {
    match phase {
        Phase::Startup => Unread::Fail(reason),
        Phase::Live => Unread::Report(reason),
    }
}

#[cfg(test)]
mod tests;
