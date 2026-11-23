//! What a seed replay does at one fixture name, decided without any I/O from what a walk of the
//! seed directory found there, what seeding recorded when it last read the name's file, and what
//! the catalog holds.
//!
//! The decision keeps three promises:
//!
//! - Content, not attributes, decides whether a file changed. A fingerprint that differs from the
//!   record's, or a record that could have missed a change, only gets the file read and hashed
//!   again, and a file whose hashes equal the record's changes nothing in the catalog.
//! - Seeding owns a [`Version`], a generation at one metageneration, only when it published it,
//!   or when its record already owned exactly that live version; it deletes only a version it
//!   owns whose body is still the one it recorded, and only where a lookup proved the fixture
//!   file gone. An API write of the body or of the metadata ends that ownership.
//! - A file that goes missing while the server runs and returns with the body seeding read never
//!   replays over an API write: the name keeps a record that owns nothing but still knows that
//!   body. Where seeding deleted its own object, the record also keeps the version it deleted,
//!   and a file that returns with the body restores the object only while no API write has
//!   taken the name since.
//! - A startup replay, which reseed shares, lets the fixture tree win over the catalog, as it did
//!   before any record existed, and fails on what it cannot replay. It also deletes, on a single
//!   proof, an object seeding purely owns whose file is gone: the file went missing while the
//!   server was stopped, and that deletion is the one change it makes beyond replaying files. The
//!   proof must come from the lookup after the walk of a recorded name the walk never listed; a
//!   name the walk listed and then found empty, as an editor's rename-save leaves it for a
//!   moment, is skipped.
//!
//! Each kind of observation has a function of its own, so that a caller receives only outcomes
//! it can meet: [`startup_file`]; [`live_file`], then [`live_read`] once the file was hashed;
//! [`absent`], then [`confirm_absence`] while the server runs; [`unproven`]; and [`unread`].
//!
//! A replay keeps a [`SeedRecord`] per replayed name (`seed_records`) in the transaction that
//! publishes it, with the version that publication assigned. Its walk looks up the recorded names
//! it did not list, since a walk lists only what is there, and the replay acts on a proof of
//! absence only once the walk ended with its final check that the root's path still names the
//! root it listed. It deletes for [`Vanished::Delete`] with both the `ifGenerationMatch` and the
//! `ifMetagenerationMatch` condition of the owned version, checked in the transaction that
//! deletes; a failed condition deletes nothing and keeps the record. A reset drops every record,
//! a reset to the startup state records what it publishes again, and a bucket deletion or purge
//! drops the records of the bucket's names, so no record claims an object one of them removed.
//!
//! A pass while the server runs (`seed_watch`) commits what it found in one transaction beside
//! the requests the server serves. It treats every record it did not observe as unknown, not
//! absent, once its walk failed or stopped at a limit, and decides again on its next pass where a
//! condition failed.

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
    Live,
}

/// What a stat of a fixture file reported, and whether a change made just after the stat could
/// leave every field it reported as it was. Both are fixed when the stat is taken, so the two
/// cannot disagree.
///
/// A record keeps the fingerprint that a read of the file took just before it hashed the bytes,
/// once a stat after the read found the file unchanged, so the fingerprint describes exactly
/// what was hashed. The stat a walk takes when it opens the file only decides, with
/// [`live_file`], whether to read the file again: the file may change between the two.
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

    /// A fingerprint as a record stored it, with the raciness it had when it was taken.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "forwards the stored raciness, without selecting an operation mode"
    )]
    pub(crate) fn stored(identity: FileIdentity, racy: bool) -> Self {
        Self { identity, racy }
    }

    /// What the stat reported.
    pub(crate) fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// Whether a change made just after the stat could have left every field it reported as it
    /// was.
    pub(crate) fn racy(&self) -> bool {
        self.racy
    }
}

/// What a fixture file holds, or an object's body: what decides whether a file changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Body {
    pub(crate) size: u64,
    pub(crate) hashes: Hashes,
}

/// A generation of an object at one metageneration. Every API write changes one of the two: a
/// new body makes a new generation, and a metadata edit a new metageneration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Version {
    pub(crate) generation: i64,
    pub(crate) metageneration: i64,
}

/// What seeding recorded at a name when a replay last read its fixture file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeedRecord {
    /// The version seeding owns at the name: the one it published, or one the record already
    /// owned when a replay found it live and holding the file's body. An API write never passes
    /// ownership to seeding, even of an identical body or of the metadata seeding wrote.
    pub(crate) owned: Option<Version>,
    /// What the file held when a replay last read it.
    pub(crate) body: Body,
    /// The fingerprint of the read that last hashed the file, or of a later stat that found it
    /// unchanged.
    pub(crate) fingerprint: Fingerprint,
    /// The run of consecutive passes while the server runs that each proved the file gone. A pass
    /// that finds an entry of any kind under the name, or cannot tell whether it holds one,
    /// clears it ([`SeedRecord::found`]); a startup replay or a reseed clears every one.
    pub(crate) absent: Option<Absence>,
    /// The version seeding deleted at the name once a lookup proved the file gone
    /// ([`SeedRecord::after_deleting`]), until a replay records the name again. A file that
    /// returns with [`Self::body`] restores the object while nothing is live at the name, and
    /// leaves any object an API write made there since.
    pub(crate) deleted: Option<Version>,
}

impl SeedRecord {
    /// The record of a file a replay read holding `body`, with the fingerprint of that read and
    /// `owned` as the version seeding owns.
    pub(crate) fn new(owned: Option<Version>, body: Body, fingerprint: Fingerprint) -> Self {
        Self {
            owned,
            body,
            fingerprint,
            absent: None,
            deleted: None,
        }
    }

    /// The record kept for [`Reread::Refresh`]: the fingerprint of the read that found the body
    /// the record knows, with the proof of absence cleared, and with any deletion of seeding's
    /// own forgotten, since the refresh happens only where an API write has taken the name since.
    pub(crate) fn refreshed(self, fingerprint: Fingerprint) -> Self {
        Self {
            fingerprint,
            absent: None,
            deleted: None,
            ..self
        }
    }

    /// The record kept once seeding deleted the version `owned` for [`Vanished::Delete`]: it owns
    /// nothing, and keeps the body and the version it deleted, so that a file that returns with
    /// that body restores the object only while no API write has taken the name since.
    pub(crate) fn after_deleting(self, owned: Version) -> Self {
        Self {
            owned: None,
            deleted: Some(owned),
            ..self
        }
    }

    /// The record a pass keeps at a name under which it found an entry of any kind, such as a
    /// file it read, one that kept failing to open or one seeding cannot use, or where it could
    /// not tell: an earlier proof of absence no longer counts toward a deletion. A pass clears
    /// the stored proof in place instead (`seed_records::found`), so that it never writes back a
    /// record a bucket deletion dropped; the decision tests state the rule with this form.
    #[cfg(test)]
    pub(crate) fn found(self) -> Self {
        Self {
            absent: None,
            ..self
        }
    }

    /// The record kept for [`Vanished::Tombstone`]: it owns nothing, and keeps the body, the
    /// fingerprint, the proofs of absence and any version seeding deleted that it had.
    pub(crate) fn tombstone(self) -> Self {
        Self {
            owned: None,
            ..self
        }
    }
}

/// What the catalog holds at a fixture name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveObject {
    /// The live generation, at its current metageneration.
    pub(crate) version: Version,
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
    /// Publish the file as the name's new live generation, whose version seeding then owns.
    Publish,
    /// Write nothing, since the live object already is what the file would write, and record the
    /// file with `owned` as the version seeding owns.
    Record { owned: Option<Version> },
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
        // write made, or that seeding wrote before it kept records, stays unowned, and so does
        // one whose metadata an API write edited and then restored.
        let owned = record
            .and_then(|record| record.owned)
            .filter(|owned| *owned == live.version);
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
/// it, until a reseed replays every file again. A file at a name where seeding deleted its own
/// object is read whatever its fingerprint: it was proven gone, so it came back, possibly
/// unchanged, as when its directory was renamed away and back.
pub(crate) fn live_file(record: Option<&SeedRecord>, observed: &Fingerprint) -> Look {
    match record {
        Some(record)
            if record.deleted.is_none()
                && !record.fingerprint.racy
                && record.fingerprint.identity == observed.identity =>
        {
            Look::Unchanged
        }
        Some(_) | None => Look::Read,
    }
}

/// What a pass while the server runs does with a fixture file it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reread {
    /// The file holds the body the record read: keep [`SeedRecord::refreshed`], with the new
    /// fingerprint and the proof of absence cleared, so that an API delete or overwrite stays.
    Refresh,
    /// The name has no record, or the file's content changed: replay it as a startup replay
    /// would.
    Changed(Replay),
}

/// What a pass while the server runs does with a fixture file it read and found holding `file`,
/// at a name where seeding recorded `record` and the catalog holds `live`. A file back with the
/// body of an object seeding itself deleted restores that object where nothing is live, and only
/// refreshes the record where an API write has taken the name since.
pub(crate) fn live_read(
    file: &Body,
    record: Option<&SeedRecord>,
    live: Option<&LiveObject>,
) -> Reread {
    match record {
        Some(known) if known.body == *file && (known.deleted.is_none() || live.is_some()) => {
            Reread::Refresh
        }
        Some(_) | None => Reread::Changed(startup_file(file, record, live)),
    }
}

/// What a replay does at a recorded name whose fixture file a lookup proved absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vanished {
    /// Delete the live object on the condition that it still is the version `owned`, then keep
    /// [`SeedRecord::after_deleting`], so that a file that returns later with the body restores
    /// the object only while nothing is live at the name. A pass while running deletes only once
    /// [`confirm_absence`] confirms it.
    Delete { owned: Version },
    /// Change nothing in the catalog, and keep the record as a tombstone
    /// ([`SeedRecord::tombstone`]) where seeding owns nothing purely at the name, as after an API
    /// write replaced or edited what it published. The tombstone keeps the body, so a file that
    /// returns with it, as after an editor's rename-save or a branch switch and back, only
    /// refreshes the record and the API write stays; one that returns with another body is a
    /// changed file. Only a pass while running keeps one.
    Tombstone,
    /// Change nothing and drop the record of a name where seeding owns nothing purely. Only a
    /// startup replay does this: the fixture tree wins at startup, so a file that returns after
    /// it is replayed as one the tree gained, as it would have been had it been there.
    Forget,
    /// Change nothing and keep the record: a hold or an unmet retention period refuses the
    /// deletion. Deleting is the one change seeding did not make before it kept records, so the
    /// deletion is skipped rather than failing a startup replay.
    Protected,
}

/// What a `phase` replay does at a name where seeding recorded `record` and the catalog holds
/// `live`, once a lookup proved the fixture file absent. A name without a record owns nothing,
/// so a caller only reports it. Seeding deletes only the version it owns, and only while its body
/// is still the one the record read: an object whose body or metadata changed since is not purely
/// what seeding wrote, and its name keeps a tombstone while the server runs.
pub(crate) fn absent(
    phase: Phase,
    _proof: &Vacant,
    record: &SeedRecord,
    live: Option<&LiveObject>,
) -> Vanished {
    let Some((owned, live)) = purely_owned(record, live) else {
        return match phase {
            Phase::Startup => Vanished::Forget,
            Phase::Live => Vanished::Tombstone,
        };
    };
    match live.protection {
        Protection::Unprotected => Vanished::Delete { owned },
        Protection::Protected => Vanished::Protected,
    }
}

/// The version seeding owns at a name where it recorded `record`, with `live`, where that version
/// is the live object and still holds the body the record read.
fn purely_owned<'a>(
    record: &SeedRecord,
    live: Option<&'a LiveObject>,
) -> Option<(Version, &'a LiveObject)> {
    record
        .owned
        .zip(live)
        .filter(|(owned, live)| live.version == *owned && live.body == record.body)
}

/// What a replay does at a recorded name the walk did not list, where the lookup after the walk
/// could not tell whether the fixture file is there: every such lookup on a platform where
/// nothing proves a name absent, and a Unix lookup that cannot search a directory above the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unproven {
    /// Seeding purely owns the live object: change nothing, and report the name. Only a proof of
    /// absence deletes the object, so the report lasts until the file is back, the object is
    /// deleted or written through the API, or a reset drops the record.
    Report,
    /// Seeding owns nothing purely at the name, as after an API write replaced, edited or deleted
    /// what it published: keep the record as a tombstone ([`SeedRecord::tombstone`]) and report
    /// nothing. A proof of absence would change nothing there either, and a file found again with
    /// the body the record knows only refreshes the record, so the name needs no attention.
    Tombstone,
}

/// What a replay of either phase does at a recorded name where seeding recorded `record` and the
/// catalog holds `live`, once the lookup after the walk could not tell whether the fixture file
/// is there. Dropping ownership needs no proof: the version seeding owned is no longer live, and
/// a later version never becomes it.
pub(crate) fn unproven(record: &SeedRecord, live: Option<&LiveObject>) -> Unproven {
    match purely_owned(record, live) {
        Some(_) => Unproven::Report,
        None => Unproven::Tombstone,
    }
}

/// How long a pass while the server runs waits between the two proofs of absence a deletion takes.
pub(crate) const ABSENCE_CONFIRMED_AFTER: Duration = Duration::from_secs(1);

/// A run of consecutive passes while the server runs that each proved a name absent: when the
/// first did, and the number of the last. Passes are numbered from 1 as the server starts them,
/// and every pass takes a number, even one that fails before it commits anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Absence {
    pub(crate) since: SystemTime,
    pub(crate) pass: u64,
}

/// Whether a pass while the server runs deletes now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirmation {
    /// Consecutive passes, the first and the last at least `ABSENCE_CONFIRMED_AFTER` apart,
    /// proved the file absent.
    Confirmed,
    /// Keep this run of proofs and delete nothing yet.
    Pending(Absence),
}

/// Whether pass number `pass`, at `now`, which proved a name absent where earlier passes kept
/// `proven`, deletes what [`absent`] told it to. While the server runs, a file can be missing
/// for a moment, as when an editor or a sync tool replaces it by unlinking it and renaming
/// another into its place, so a deletion waits for a second proof at least
/// `ABSENCE_CONFIRMED_AFTER` after the first. The proofs count only from consecutive passes: a
/// pass in between that found an entry under the name, or could not tell, cleared the run
/// ([`SeedRecord::found`]), and one that failed before it committed left a run whose last pass is
/// not the one before `pass`. Either way the proof starts the wait again. Pass numbers start over
/// when the server restarts, which only starts the wait over: the startup replay clears every
/// run. A startup replay deletes on one proof: the file went missing while the server was
/// stopped.
pub(crate) fn confirm_absence(proven: Option<Absence>, pass: u64, now: SystemTime) -> Confirmation {
    // A first proof, a run broken by a failed pass, or one dated after `now` by a clock that
    // moved back, starts the wait.
    let since = proven
        .filter(|proven| proven.pass.checked_add(1) == Some(pass) && proven.since <= now)
        .map_or(now, |proven| proven.since);
    if now.duration_since(since).unwrap_or_default() >= ABSENCE_CONFIRMED_AFTER {
        Confirmation::Confirmed
    } else {
        Confirmation::Pending(Absence { since, pass })
    }
}

/// What a replay does at a name it could not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unread<R> {
    /// Fail the whole replay with the reason, which then changes nothing.
    Fail(R),
    /// Change nothing at the name, keep its record with any proof of absence cleared
    /// ([`SeedRecord::found`]), and report the reason, so that a name that could not be read
    /// never passes for one found unchanged, nor for one still missing.
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
