//! What a replay does at one fixture name, decided without any I/O from what the walk found
//! there, what seeding recorded when it last replayed the name, and what the catalog holds.
//!
//! The decision keeps two promises. Seeding deletes only a generation it wrote that is still
//! live, and only where a lookup proved the fixture file gone: an entry that could not be
//! opened, listed or read steadily is unknown, never absent. And a startup replay, which
//! reseed shares, keeps the contract it had before any record existed: the fixture tree wins
//! over the catalog, and what cannot be replayed fails it.

use crate::{fsutil::Vacant, import::Fingerprint};
use std::time::SystemTime;

/// Which replay decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The replay before the listener binds, and a reseed. Every fixture file is replayed over
    /// the catalog whatever its record says, and anything that cannot be replayed fails it.
    Startup,
    /// A pass while the server runs. Only a file that changed since its record is replayed, so
    /// an API delete or overwrite stays until the file changes, and nothing fails the pass.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pass that watches the seed directory while the server runs is not wired yet"
        )
    )]
    Live,
}

/// What the walk found at a fixture name.
#[derive(Debug)]
pub(crate) enum Observation {
    /// A regular file, with the fingerprint a stat of it reported.
    File(Fingerprint),
    /// `fstatat`, not following a symlink, found nothing under the name. Only a lookup makes
    /// the proof, so a name that failed to open in any other way never passes for absent.
    Absent(Vacant),
    /// Whether the name holds a file is unknown: it, or a directory above it, could not be
    /// opened, listed or read without changing.
    Unknown,
    /// An entry that seeding never imports.
    Unusable(Unusable),
}

/// Why seeding cannot use an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unusable {
    /// A symlink, which seeding never follows.
    Symlink,
    /// Neither a regular file nor a directory, such as a socket or a FIFO.
    Special,
    /// A name that is not UTF-8.
    NonUtf8Name,
    /// A name that is no valid bucket or object name.
    InvalidName,
}

/// What seeding recorded when it last replayed a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SeedRecord {
    /// The generation that replay left live.
    generation: i64,
    /// The fixture file's fingerprint when that replay observed it.
    fingerprint: Fingerprint,
    /// Whether that observation could miss a change made just after it
    /// ([`Fingerprint::racy_at`]).
    racy: bool,
}

impl SeedRecord {
    /// The record of a replay that left `generation` live from a file whose stat, taken no
    /// later than `observed` on the injected clock, reported `fingerprint`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the seed record table that keeps these is not wired yet, so every replay passes none"
        )
    )]
    pub(crate) fn new(generation: i64, fingerprint: Fingerprint, observed: SystemTime) -> Self {
        let racy = fingerprint.racy_at(observed);
        Self {
            generation,
            fingerprint,
            racy,
        }
    }

    /// Whether the file may have changed since this record: it shows another fingerprint, or
    /// the record was racy, so that a second write within the same timestamp tick, which
    /// leaves the fingerprint as it was, is read again.
    fn changed(&self, observed: &Fingerprint) -> bool {
        self.racy || !observed.same_file_state(&self.fingerprint)
    }
}

/// What the catalog holds at a fixture name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveObject {
    /// The live generation.
    pub(crate) generation: i64,
    pub(crate) content: Content,
    pub(crate) protection: Protection,
}

/// Whether the live object is what replaying the fixture file would write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    /// The fixture's size, MD5 and CRC32C, the seed content type and no other metadata.
    AsSeeded,
    /// Anything else, such as an API overwrite or an API metadata edit.
    Different,
}

/// Whether a hold or an unmet retention period refuses the change a replay would make to the
/// live object: replacing it for a file, deleting it for a name proven absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protection {
    Unprotected,
    Protected,
}

/// What a replay does at one fixture name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Publish the fixture file as the name's new live generation, and record it.
    Publish,
    /// Write nothing, since the live object already is what the file would write, and record
    /// the file.
    Record,
    /// Delete the live object only while `generation`, the one seeding wrote, is still live, and
    /// drop the record.
    Delete { generation: i64 },
    /// Change nothing, and keep the record.
    Keep,
    /// Change nothing, and drop the record.
    Forget,
    /// Change nothing, keep the record, and report why the name was skipped.
    Skip(Skip),
    /// Fail the whole replay, which then changes nothing.
    Fail,
}

/// Why a replay left a name as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    /// A hold or an unmet retention period refuses the change.
    Protected,
    /// Seeding cannot use the entry.
    Unusable(Unusable),
}

/// Decides what a `phase` replay does at a fixture name where the walk found `observation`,
/// seeding last recorded `record` and the catalog holds `live`.
pub(crate) fn decide(
    phase: Phase,
    observation: &Observation,
    record: Option<&SeedRecord>,
    live: Option<LiveObject>,
) -> Outcome {
    match observation {
        Observation::File(fingerprint) => file(phase, fingerprint, record, live),
        Observation::Absent(_) => absent(record, live),
        Observation::Unknown => match phase {
            Phase::Startup => Outcome::Fail,
            // The next pass looks again; until then the record keeps what seeding wrote.
            Phase::Live => Outcome::Keep,
        },
        Observation::Unusable(reason) => match phase {
            Phase::Startup => Outcome::Fail,
            Phase::Live => Outcome::Skip(Skip::Unusable(*reason)),
        },
    }
}

fn file(
    phase: Phase,
    fingerprint: &Fingerprint,
    record: Option<&SeedRecord>,
    live: Option<LiveObject>,
) -> Outcome {
    let replayed = match phase {
        Phase::Startup => true,
        Phase::Live => record.is_none_or(|record| record.changed(fingerprint)),
    };
    if !replayed {
        return Outcome::Keep;
    }
    let Some(live) = live else {
        return Outcome::Publish;
    };
    match (live.content, live.protection) {
        // Nothing would change, so protection refuses nothing.
        (Content::AsSeeded, Protection::Unprotected | Protection::Protected) => Outcome::Record,
        (Content::Different, Protection::Unprotected) => Outcome::Publish,
        (Content::Different, Protection::Protected) => match phase {
            Phase::Startup => Outcome::Fail,
            Phase::Live => Outcome::Skip(Skip::Protected),
        },
    }
}

/// Deleting is the one change seeding did not make before records existed, so a protected
/// object skips the deletion in both phases instead of failing a startup replay.
fn absent(record: Option<&SeedRecord>, live: Option<LiveObject>) -> Outcome {
    // Without a record, or with one whose generation is no longer live, any live object is not
    // the one seeding wrote: an API write replaced it, or seeding wrote it before records.
    let Some((record, live)) = record
        .zip(live)
        .filter(|(record, live)| record.generation == live.generation)
    else {
        return Outcome::Forget;
    };
    match live.protection {
        Protection::Unprotected => Outcome::Delete {
            generation: record.generation,
        },
        Protection::Protected => Outcome::Skip(Skip::Protected),
    }
}

#[cfg(test)]
mod tests;
