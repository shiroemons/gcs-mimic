//! Opens the child of a directory under its exact spelling, and tells an entry the filesystem
//! reached under another letter case or Unicode normalization apart, with the spelling the
//! directory lists a regular file under.
//!
//! Whether a name that opens has that spelling is something only some directories say. macOS
//! reports an open file's own name; Linux reports a directory's casefold flag, and a directory
//! whose flag is clear keeps names apart; elsewhere, and where neither answers, only a listing of
//! the parent shows the spelling. What a probe of the root found only makes that check stricter,
//! never looser: on a root that folds letter case or normalization, a clear casefold flag is not
//! believed, because a mount that folds without that flag, such as a Docker Desktop bind mount,
//! reports the same. On a root that folds neither, each directory still answers for itself. The
//! probe measured one directory the mirror created, and a mount can fold names per directory
//! without saying so: on a WSL2 `drvfs` mount with `case=dir`, a directory created from Windows
//! folds case while one created from Linux does not, on one device.
//!
//! A listing is read in one pass over the directory and kept for the lookups that follow in the
//! same pass of the synchronization, such as one ingestion rescan, one deletion inventory or one
//! installation: a lookup and its clones share what they read. A kept listing answers only while
//! its directory reports the identity, size and timestamps it had before the listing was read.
//! Creating, removing or renaming an entry changes the directory's modification and change
//! times, so a directory that changed during the pass is read again rather than judged by its
//! old names. The directory is asked past the attribute cache a Linux FUSE or NFS mount keeps
//! ([`imp::uncached_identity`]), which a rename made on the host does not invalidate. A listing
//! is kept only when the directory reported the same before and after it was read, and only once
//! the directory's last change is old enough that a later one cannot fall in the same timestamp
//! tick ([`FileIdentity::settled_at`]); a directory that just changed is read again for each
//! lookup. That age compares this host's clock with timestamps the filesystem's own host wrote,
//! so a clock running ahead of the file server's keeps a listing sooner. Each lookup opens its
//! entry before it asks, so a kept listing that still answers was read from the namespace the
//! entry was opened in.

use super::{Child, DirectoryOpen, FileIdentity, Opened, RegularChild, imp};
use crate::clock::{Clock, SystemClock, signed_nanos};
use std::{
    collections::{HashSet, VecDeque},
    ffi::{OsStr, OsString},
    fs::File,
    io,
    ops::ControlFlow,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// The most directories one pass keeps listings of. Passes walk paths in order, so the
/// directories they look up again are the few they just read.
const KEPT_LISTINGS: usize = 4;
/// How long one lookup may wait in all for names that `fstatat` still finds while they keep
/// failing to open with `ENOENT`, before it gives up on each further such name at once. A Docker
/// Desktop bind mount answered so for up to 238 ms after the host replaced a file
/// (`docs/en/evidence/sync-bind-mount-stale-reads.md`). A file job is handed its own lookup, so
/// a job that meets many such names, such as a scan of a tree the host just rewrote, is held up
/// for this long at most rather than this long for each name.
const WAIT_BUDGET: Duration = Duration::from_secs(1);
/// The most names one kept listing holds, which bounds its memory with the name length limit. A
/// larger directory is read again for each lookup, up to the name looked up.
const KEPT_NAMES: usize = 4096;

/// How the filesystem of a probed root matched a name the probe opened against the names its
/// directory lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Matching {
    /// Names that differ in letter case or Unicode normalization named different files.
    Exact,
    /// A name opened an entry that its directory lists under another letter case or
    /// normalization.
    Folding,
}

/// What a lookup knows about the directories it opens names in, beyond each directory's own
/// answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Known {
    /// Nothing: only the directory's own answer tells, as below a root the probe found to fold
    /// nothing, outside a synchronization root, or for the probe itself.
    Nothing,
    /// The directory lies below a root that folds letter case or normalization.
    Folding,
}

/// Opens children under the naming rules a probe found for the root they lie below. Clones share
/// the listings they read, and each starts with its own [`WAIT_BUDGET`].
#[derive(Clone)]
pub(crate) struct Lookup {
    #[cfg_attr(
        not(any(target_os = "linux", all(test, unix))),
        expect(
            dead_code,
            reason = "only a Linux casefold flag answers a spelling that the root's rules can overrule"
        )
    )]
    known: Known,
    listings: Arc<Mutex<Listings>>,
    /// Tells when a listing was read, to keep only listings of a directory that has settled.
    clock: Arc<dyn Clock>,
    waited: Waited,
}

/// The time a lookup spent waiting for names to open again. A clone starts with none spent, so
/// that one handed to a file job is not held up by the waits of another.
#[derive(Debug, Default)]
struct Waited(AtomicU64);

impl Clone for Waited {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl Waited {
    fn spent(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::Relaxed))
    }

    fn add(&self, wait: Duration) {
        // A wait never exceeds the budget of a second, so its nanoseconds fit.
        let nanos = u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX);
        self.0.fetch_add(nanos, Ordering::Relaxed);
    }
}

/// The listings a pass keeps, oldest first.
#[derive(Debug, Default)]
struct Listings {
    kept: VecDeque<Listing>,
    /// How many times the pass read a directory to confirm a spelling.
    #[cfg(test)]
    reads: usize,
}

/// The names of one directory, with what the directory reported before they were read.
#[derive(Debug)]
struct Listing {
    state: FileIdentity,
    names: HashSet<OsString>,
}

impl Listings {
    fn find(&self, state: FileIdentity) -> Option<&Listing> {
        self.kept.iter().find(|listing| listing.state == state)
    }

    /// Keeps `listing` in place of any older one of its directory, dropping the oldest beyond
    /// [`KEPT_LISTINGS`].
    fn keep(&mut self, listing: Listing) {
        self.kept
            .retain(|kept| kept.state.node() != listing.state.node());
        if self.kept.len() == KEPT_LISTINGS {
            self.kept.pop_front();
        }
        self.kept.push_back(listing);
    }
}

impl Lookup {
    /// Lookups that know nothing about the filesystem: each directory's own answer decides.
    pub(crate) fn unprobed() -> Self {
        Self::unprobed_on(Arc::new(SystemClock))
    }

    /// Like [`Self::unprobed`], telling the time and waiting on `clock`.
    pub(crate) fn unprobed_on(clock: Arc<dyn Clock>) -> Self {
        Self::with(Known::Nothing, clock)
    }

    /// Lookups below a root whose probe matched names as `matching` says, telling the time of a
    /// listing by `clock`.
    pub(crate) fn probed(matching: Matching, clock: Arc<dyn Clock>) -> Self {
        let known = match matching {
            Matching::Exact => Known::Nothing,
            Matching::Folding => Known::Folding,
        };
        Self::with(known, clock)
    }

    fn with(known: Known, clock: Arc<dyn Clock>) -> Self {
        Self {
            known,
            listings: Arc::default(),
            clock,
            waited: Waited::default(),
        }
    }

    /// How many times this lookup and its clones read a directory to confirm a spelling.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn reads(&self) -> usize {
        self.listings().reads
    }

    /// Keeps `names` as the listing of `parent` while `parent` reports what it does now, as a
    /// listing read just before a change that left the directory's timestamps as they were.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn keep_listing(&self, parent: &File, names: &[&str]) -> io::Result<()> {
        let state = imp::uncached_identity(parent)?;
        self.listings().keep(Listing {
            state,
            names: names.iter().map(OsString::from).collect(),
        });
        Ok(())
    }

    /// Drops the kept listing of `parent`, which a listing read since showed to be older than
    /// what the directory holds.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "descriptor-relative child lookup is unsupported on this platform"
        )
    )]
    pub(super) fn forget(&self, parent: &File) -> io::Result<()> {
        let node = super::stat_identity(parent)?.node();
        self.listings()
            .kept
            .retain(|kept| kept.state.node() != node);
        Ok(())
    }

    /// A panic while a listing was read or kept leaves the kept listings unknown, so they are
    /// dropped rather than trusted.
    fn listings(&self) -> MutexGuard<'_, Listings> {
        self.listings.lock().unwrap_or_else(|poisoned| {
            self.listings.clear_poison();
            let mut listings = poisoned.into_inner();
            listings.kept.clear();
            listings
        })
    }

    /// Whether the listing of `parent` holds `name` exactly, from a kept listing while `parent`
    /// reports what it did when that listing was read.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "descriptor-relative child lookup is unsupported on this platform"
        )
    )]
    pub(super) fn listed(&self, parent: &File, name: &OsStr) -> io::Result<bool> {
        let read_ns = signed_nanos(self.clock.now());
        let state = imp::uncached_identity(parent)?;
        if let Some(listing) = self.listings().find(state) {
            return Ok(listing.names.contains(name));
        }
        #[cfg(test)]
        {
            self.listings().reads += 1;
        }
        // The lock is not held while the directory is read, so lookups of other directories by
        // clones on other threads do not wait for it.
        let mut names = Some(HashSet::new());
        let mut found = false;
        imp::each_name(parent, |listed| {
            found |= listed == name;
            // A directory with more names than a listing keeps is read again for each lookup, so
            // its names are dropped once they reach the bound.
            if names.as_ref().is_some_and(|kept| kept.len() >= KEPT_NAMES) {
                names = None;
            }
            if let Some(kept) = names.as_mut() {
                kept.insert(listed.to_owned());
            }
            Ok(if found && names.is_none() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        })?;
        if let Some(names) = names
            && state.settled_at(read_ns)
            && imp::uncached_identity(parent)? == state
        {
            self.listings().keep(Listing { state, names });
        }
        Ok(found)
    }

    /// Opens the directory `name` in `parent`, creating it when `open` allows, without following
    /// a symlink. An entry reached under another spelling is [`Child::Alias`].
    pub(crate) fn directory(
        &self,
        parent: &File,
        name: &OsStr,
        open: DirectoryOpen,
    ) -> io::Result<Child> {
        imp::child_directory(parent, name, open, self)
    }

    /// Opens the regular file `name` in `parent` without following a symlink. A file reached
    /// under another spelling is [`RegularChild::Respelled`], with the spelling `parent` lists it
    /// under.
    pub(crate) fn regular(&self, parent: &File, name: &OsStr) -> io::Result<RegularChild> {
        imp::child_regular(parent, name, self)
    }

    /// Opens the regular file `name` in `parent` without following a symlink or confirming its
    /// spelling, for a file the mirror names itself, under the rule [`Self::regular`] applies to
    /// a name that fails to open ([`Opened`]).
    pub(crate) fn regular_unconfirmed(
        &self,
        parent: &File,
        name: &OsStr,
    ) -> io::Result<Opened<File>> {
        imp::regular_unconfirmed(parent, name, self)
    }

    /// Opens the existing directory `name` in `parent` without following a symlink or confirming
    /// its spelling, for a name a listing gave or the mirror names itself, under the same rule
    /// ([`Opened`]).
    pub(crate) fn directory_unconfirmed(
        &self,
        parent: &File,
        name: &OsStr,
    ) -> io::Result<Opened<File>> {
        imp::directory_unconfirmed(parent, name, self)
    }

    /// Waits `duration` on the lookup's clock before a name that failed to open is opened again,
    /// or less where that is all that is left of [`WAIT_BUDGET`]. `false` when nothing is left:
    /// the caller gives the name up at once.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "descriptor-relative child lookup is unsupported on this platform"
        )
    )]
    pub(super) fn pause(&self, duration: Duration) -> bool {
        let left = WAIT_BUDGET.saturating_sub(self.waited.spent());
        if left.is_zero() {
            return false;
        }
        let wait = duration.min(left);
        self.clock.pause(wait);
        self.waited.add(wait);
        true
    }

    /// What the root's rules say about the directories below it.
    #[cfg_attr(
        not(any(target_os = "linux", all(test, unix))),
        expect(
            dead_code,
            reason = "only a Linux casefold flag answers a spelling that the root's rules can overrule"
        )
    )]
    pub(super) fn known(&self) -> Known {
        self.known
    }
}

/// Lookups below a folding root in a directory whose entries the platform takes as spelled the
/// way they were opened ([`super::UncheckedSpelling`]), so that every lookup there confirms its
/// spelling from a listing, which the lookup counts.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod listing_tests {
    use super::*;
    use crate::{clock::ManualClock, fsutil::UncheckedSpelling, test_support::TestDir};
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    /// Lookups whose clock reads `now`.
    fn lookup_at(now: SystemTime) -> Lookup {
        Lookup::probed(Matching::Folding, Arc::new(ManualClock::new(now)))
    }

    /// Lookups of a pass that starts long after the directory last changed.
    fn settled() -> Lookup {
        lookup_at(SystemTime::now() + Duration::from_secs(3600))
    }

    /// Fields drop in order, so the directory goes once nothing holds it open.
    struct Listed {
        parent: File,
        _unchecked: UncheckedSpelling,
        dir: TestDir,
    }

    impl Listed {
        fn new(names: &[&str]) -> Self {
            let dir = TestDir::new();
            for name in names {
                fs::write(dir.0.join(name), b"").unwrap();
            }
            let parent = File::open(&dir.0).unwrap();
            let _unchecked = UncheckedSpelling::of(&dir.0);
            Self {
                parent,
                _unchecked,
                dir,
            }
        }

        /// Dates the directory's last modification an hour back, as if it had settled before the
        /// pass, so that a change made now shows in its modification time even where timestamps
        /// are coarser than the time between two of the test's changes.
        fn settle(&self) {
            self.parent
                .set_modified(SystemTime::now() - Duration::from_secs(3600))
                .unwrap();
        }

        fn opens(&self, lookup: &Lookup, name: &str) -> bool {
            matches!(
                lookup.regular(&self.parent, name.as_ref()).unwrap(),
                RegularChild::File(_)
            )
        }
    }

    /// One pass reads an unchanged directory once for all its lookups, its clones included, and
    /// still tells another spelling apart from the kept listing; a filesystem that keeps names
    /// apart does not open that spelling at all.
    #[test]
    fn a_pass_reads_an_unchanged_directory_once() {
        let names = ["File-0", "File-1", "File-2", "File-3"];
        let listed = Listed::new(&names);
        let lookup = settled();
        for name in names {
            assert!(listed.opens(&lookup, name), "{name}");
            assert!(listed.opens(&lookup.clone(), name), "{name}");
        }
        assert!(!listed.opens(&lookup, "file-0"));
        assert_eq!(lookup.reads(), 1);
        let next = settled();
        assert!(listed.opens(&next, "File-0"));
        assert_eq!(next.reads(), 1);
    }

    /// An entry renamed under another spelling or created during the pass is judged by a new
    /// listing, not by the kept one: the new spelling opens, the old one is an alias or missing,
    /// and the created entry opens instead of passing for an alias.
    #[test]
    fn a_directory_changed_during_a_pass_is_read_again() {
        let listed = Listed::new(&["Actual"]);
        listed.settle();
        let lookup = settled();
        assert!(listed.opens(&lookup, "Actual"));
        assert_eq!(lookup.reads(), 1);
        fs::rename(listed.dir.0.join("Actual"), listed.dir.0.join("actual")).unwrap();
        assert!(listed.opens(&lookup, "actual"));
        assert!(!listed.opens(&lookup, "Actual"));
        assert_eq!(lookup.reads(), 2);
        listed.settle();
        assert!(listed.opens(&lookup, "actual"));
        assert_eq!(lookup.reads(), 3);
        fs::write(listed.dir.0.join("Added"), b"").unwrap();
        assert!(listed.opens(&lookup, "Added"));
        assert_eq!(lookup.reads(), 4);
    }

    /// On a local filesystem, which keeps no attribute cache to bypass, the identity a listing is
    /// checked against is the one `fstat` reports, device number included, and a rename in the
    /// directory changes it.
    #[test]
    fn a_directory_reports_its_uncached_identity_as_fstat_does() {
        let listed = Listed::new(&["Actual"]);
        listed.settle();
        let before = imp::uncached_identity(&listed.parent).unwrap();
        assert_eq!(before, super::super::stat_identity(&listed.parent).unwrap());
        fs::rename(listed.dir.0.join("Actual"), listed.dir.0.join("actual")).unwrap();
        let after = imp::uncached_identity(&listed.parent).unwrap();
        assert_eq!(after, super::super::stat_identity(&listed.parent).unwrap());
        assert_ne!(after, before);
    }

    /// A directory with more names than a kept listing holds is read for each lookup, and still
    /// answers each one.
    #[test]
    fn a_directory_beyond_the_kept_names_is_read_for_each_lookup() {
        let names: Vec<String> = (0..=KEPT_NAMES)
            .map(|index| format!("File-{index}"))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let listed = Listed::new(&names);
        let lookup = settled();
        assert!(listed.opens(&lookup, "File-0"));
        assert!(listed.opens(&lookup, &format!("File-{KEPT_NAMES}")));
        assert_eq!(lookup.reads(), 2);
    }

    /// A directory whose last change lies within the coarsest timestamp tick of the pass is read
    /// for each lookup, since a change in that tick could leave its timestamps as they were.
    #[test]
    fn a_directory_that_just_changed_is_read_for_each_lookup() {
        let listed = Listed::new(&["Actual"]);
        let changed = listed.parent.metadata().unwrap().modified().unwrap();
        let lookup = lookup_at(changed + Duration::from_secs(1));
        assert!(listed.opens(&lookup, "Actual"));
        assert!(listed.opens(&lookup, "Actual"));
        assert_eq!(lookup.reads(), 2);
    }

    /// A kept listing from before a change that left the directory's timestamps as they were
    /// gives way to the listing read to name the spelling: an entry that listing spells exactly
    /// as it was opened is the file, and the next lookup reads the directory again.
    #[test]
    fn a_kept_listing_older_than_the_directory_gives_way_to_the_one_read_again() {
        let listed = Listed::new(&["Actual"]);
        listed.settle();
        let lookup = settled();
        lookup.keep_listing(&listed.parent, &["actual"]).unwrap();
        assert!(listed.opens(&lookup, "Actual"));
        assert_eq!(lookup.reads(), 0);
        assert!(listed.opens(&lookup, "Actual"));
        assert_eq!(lookup.reads(), 1);
    }
}

/// Opens that fail with `ENOENT` while the name still holds its entry, as a Docker Desktop bind
/// mount answers for a file it is still bringing over from the host.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod open_failure_tests {
    use super::*;
    use crate::{clock::ManualClock, fsutil::FailingOpens, test_support::TestDir};
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    /// A directory holding a regular file `file` and a directory `directory`, and lookups whose
    /// waits go by `clock` rather than by the time that passes.
    struct Entries {
        parent: File,
        clock: Arc<ManualClock>,
        start: SystemTime,
        dir: TestDir,
    }

    impl Entries {
        fn new() -> Self {
            let dir = TestDir::new();
            fs::write(dir.0.join("file"), b"bytes").unwrap();
            fs::create_dir(dir.0.join("directory")).unwrap();
            let start = SystemTime::now();
            Self {
                parent: File::open(&dir.0).unwrap(),
                clock: Arc::new(ManualClock::new(start)),
                start,
                dir,
            }
        }

        fn lookup(&self) -> Lookup {
            Lookup::probed(Matching::Exact, self.clock.clone())
        }

        fn waited(&self) -> Duration {
            self.clock.now().duration_since(self.start).unwrap()
        }
    }

    /// An entry whose open fails a few times while the name still holds it is opened once the
    /// open succeeds again, a regular file and a directory alike.
    #[test]
    fn an_entry_that_briefly_fails_to_open_is_opened_again() {
        let entries = Entries::new();
        let file = FailingOpens::vanishing(&entries.dir.0, "file", 3);
        assert!(matches!(
            entries
                .lookup()
                .regular(&entries.parent, "file".as_ref())
                .unwrap(),
            RegularChild::File(_)
        ));
        assert_eq!(file.left(), 0);
        let directory = FailingOpens::vanishing(&entries.dir.0, "directory", 3);
        assert!(matches!(
            entries
                .lookup()
                .directory(
                    &entries.parent,
                    "directory".as_ref(),
                    DirectoryOpen::Existing
                )
                .unwrap(),
            Child::Directory { .. }
        ));
        assert_eq!(directory.left(), 0);
    }

    /// An entry whose open keeps failing while the name still holds it is transient, never
    /// missing, once the retries spent a second on the lookup's clock; a name that holds nothing
    /// is missing at once, without a wait.
    #[test]
    fn an_entry_that_keeps_failing_to_open_is_never_missing() {
        let entries = Entries::new();
        let _file = FailingOpens::vanishing(&entries.dir.0, "file", usize::MAX);
        let _directory = FailingOpens::vanishing(&entries.dir.0, "directory", usize::MAX);
        assert!(matches!(
            entries.lookup().regular(&entries.parent, "file".as_ref()),
            Ok(RegularChild::Transient)
        ));
        assert!(entries.waited() >= Duration::from_secs(1));
        assert!(matches!(
            entries.lookup().directory(
                &entries.parent,
                "directory".as_ref(),
                DirectoryOpen::Existing
            ),
            Ok(Child::Transient)
        ));
        assert!(matches!(
            entries
                .lookup()
                .regular_unconfirmed(&entries.parent, "file".as_ref()),
            Ok(Opened::Transient)
        ));
        let waited = entries.waited();
        assert!(matches!(
            entries
                .lookup()
                .regular(&entries.parent, "absent".as_ref())
                .unwrap(),
            RegularChild::Missing(_)
        ));
        assert_eq!(entries.waited(), waited);
    }

    /// The waits of one lookup, which a file job is handed, add up to a second at most: once it
    /// has waited that long, an entry that fails to open while its name still holds it is
    /// transient at once, and a name that holds nothing is still missing. A clone, handed to
    /// another job, waits again.
    #[test]
    fn a_lookup_waits_a_second_at_most_for_all_its_names() {
        let entries = Entries::new();
        let _file = FailingOpens::vanishing(&entries.dir.0, "file", usize::MAX);
        let _directory = FailingOpens::vanishing(&entries.dir.0, "directory", usize::MAX);
        let lookup = entries.lookup();
        assert!(matches!(
            lookup.regular(&entries.parent, "file".as_ref()),
            Ok(RegularChild::Transient)
        ));
        let spent = entries.waited();
        assert!(spent <= Duration::from_secs(1), "{spent:?}");
        assert!(matches!(
            lookup.directory(
                &entries.parent,
                "directory".as_ref(),
                DirectoryOpen::Existing
            ),
            Ok(Child::Transient)
        ));
        assert!(matches!(
            lookup.regular(&entries.parent, "absent".as_ref()).unwrap(),
            RegularChild::Missing(_)
        ));
        assert_eq!(entries.waited(), spent);
        let other = lookup.clone();
        assert!(matches!(
            other.regular_unconfirmed(&entries.parent, "file".as_ref()),
            Ok(Opened::Transient)
        ));
        let waited = entries.waited();
        assert!(waited > spent);
        assert!(matches!(
            lookup.regular_unconfirmed(&entries.parent, "file".as_ref()),
            Ok(Opened::Transient)
        ));
        assert_eq!(entries.waited(), waited);
    }
}
