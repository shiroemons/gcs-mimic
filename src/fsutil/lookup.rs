//! Opens the child of a directory under its exact spelling, and tells an entry the filesystem
//! reached under another letter case or Unicode normalization apart as an alias.
//!
//! Whether a name that opens has that spelling is something only some directories say. macOS
//! reports an open file's own name; Linux reports a directory's casefold flag, and a directory
//! whose flag is clear keeps names apart; elsewhere, and where neither answers, only a listing of
//! the parent shows the spelling. What a probe of the root found decides where those answers are
//! not needed or not believed: on a root whose filesystem folds neither letter case nor
//! normalization, a name that opens on the root's device is spelled that way; on a root that
//! folds either, a clear casefold flag is not believed, because a mount that folds without that
//! flag, such as a Docker Desktop bind mount, reports the same. A directory's own casefold flag
//! still wins over the root's rules: ext4 sets it per directory, so a root that keeps names apart
//! can hold a directory that folds them.
//!
//! A listing is read in one pass over the directory and kept for the lookups that follow in the
//! same pass of the synchronization, such as one ingestion rescan, one deletion inventory or one
//! installation: a lookup and its clones share what they read. A kept listing answers only while
//! its directory reports the identity, size and timestamps it had before the listing was read.
//! Creating, removing or renaming an entry changes the directory's modification and change
//! times, so a directory that changed during the pass is read again rather than judged by its
//! old names. A listing is kept only when the directory reported the same before and after it was
//! read, and only once the directory's last change is old enough that a later one cannot fall in
//! the same timestamp tick ([`FileIdentity::settled_at`]); a directory that just changed is read
//! again for each lookup. Each lookup opens its entry before it asks, so a kept listing that
//! still answers was read from the namespace the entry was opened in.

use super::{Child, DirectoryOpen, FileIdentity, RegularChild, imp, stat_identity};
use crate::clock::{Clock, SystemClock, signed_nanos};
use std::{
    collections::{HashSet, VecDeque},
    ffi::{OsStr, OsString},
    fs::File,
    io,
    ops::ControlFlow,
    sync::{Arc, Mutex, MutexGuard},
};

/// The most directories one pass keeps listings of. Passes walk paths in order, so the
/// directories they look up again are the few they just read.
const KEPT_LISTINGS: usize = 4;
/// The most names one kept listing holds, which bounds its memory with the name length limit. A
/// larger directory is read again for each lookup, up to the name looked up.
const KEPT_NAMES: usize = 4096;

/// How the filesystem of a probed root matches a name it opens against the names its directories
/// list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Matching {
    /// Names that differ in letter case or Unicode normalization name different files.
    Exact,
    /// A name can open an entry that its directory lists under another letter case or
    /// normalization.
    Folding,
}

/// What a lookup knows about the filesystem it opens names on, beyond each directory's own answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rules {
    /// Nobody probed it, as for a lookup outside a synchronization root or the probe itself.
    Unprobed,
    /// The root on `device` folds neither letter case nor normalization.
    Exact { device: u64 },
    /// The root folds letter case or normalization.
    Folding,
}

/// What the rules of a lookup say about one directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Known {
    /// Nothing: only the directory's own answer tells.
    Nothing,
    /// The directory lies on the device of a root that folds neither letter case nor
    /// normalization.
    Exact,
    /// The directory lies below a root that folds letter case or normalization.
    Folding,
}

/// Opens children under the naming rules a probe found for the root they lie below. Clones share
/// the listings they read.
#[derive(Clone)]
pub(crate) struct Lookup {
    rules: Rules,
    listings: Arc<Mutex<Listings>>,
    /// Tells when a listing was read, to keep only listings of a directory that has settled.
    clock: Arc<dyn Clock>,
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
        Self::with(Rules::Unprobed, Arc::new(SystemClock))
    }

    /// Lookups below the root on `device` whose filesystem matches names as `matching` says,
    /// telling the time of a listing by `clock`.
    pub(crate) fn probed(device: u64, matching: Matching, clock: Arc<dyn Clock>) -> Self {
        let rules = match matching {
            Matching::Exact => Rules::Exact { device },
            Matching::Folding => Rules::Folding,
        };
        Self::with(rules, clock)
    }

    fn with(rules: Rules, clock: Arc<dyn Clock>) -> Self {
        Self {
            rules,
            listings: Arc::default(),
            clock,
        }
    }

    /// How many times this lookup and its clones read a directory to confirm a spelling.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn reads(&self) -> usize {
        self.listings().reads
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
        let state = stat_identity(parent)?;
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
            names = names
                .take()
                .filter(|kept| kept.len() < KEPT_NAMES)
                .map(|mut kept| {
                    kept.insert(listed.to_owned());
                    kept
                });
            Ok(if found && names.is_none() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        })?;
        if let Some(names) = names
            && state.settled_at(read_ns)
            && stat_identity(parent)? == state
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

    /// Opens the regular file `name` in `parent` without following a symlink. An entry reached
    /// under another spelling is [`RegularChild::Alias`].
    pub(crate) fn regular(&self, parent: &File, name: &OsStr) -> io::Result<RegularChild> {
        imp::child_regular(parent, name, self)
    }

    /// What the rules say about `directory`. Only a root that keeps names apart needs the
    /// directory's device: a mount below it can fold them.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "descriptor-relative child lookup is unsupported on this platform"
        )
    )]
    pub(super) fn known(&self, directory: &File) -> io::Result<Known> {
        Ok(match self.rules {
            Rules::Folding => Known::Folding,
            Rules::Exact { device } if stat_identity(directory)?.device == device => Known::Exact,
            Rules::Unprobed | Rules::Exact { .. } => Known::Nothing,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The rules of a root that keeps names apart cover only directories on its device, since a
    /// mount below it can fold names; the rules of a root that folds them cover every directory.
    #[test]
    fn only_the_root_device_takes_the_rules_of_a_root_that_keeps_names_apart() {
        let dir = crate::test_support::TestDir::new();
        let directory = File::open(&dir.0).unwrap();
        let device = stat_identity(&directory).unwrap().device;
        let other = device.wrapping_add(1);
        let known = |device, matching| {
            Lookup::probed(device, matching, Arc::new(SystemClock))
                .known(&directory)
                .unwrap()
        };
        assert_eq!(known(device, Matching::Exact), Known::Exact);
        assert_eq!(known(other, Matching::Exact), Known::Nothing);
        assert_eq!(known(other, Matching::Folding), Known::Folding);
        assert_eq!(
            Lookup::unprobed().known(&directory).unwrap(),
            Known::Nothing
        );
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

        /// Lookups whose clock reads `now`.
        fn lookup(&self, now: SystemTime) -> Lookup {
            let device = stat_identity(&self.parent).unwrap().device;
            Lookup::probed(device, Matching::Folding, Arc::new(ManualClock::new(now)))
        }

        /// Lookups of a pass that starts long after the directory last changed.
        fn settled(&self) -> Lookup {
            self.lookup(SystemTime::now() + Duration::from_secs(3600))
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
        let lookup = listed.settled();
        for name in names {
            assert!(listed.opens(&lookup, name), "{name}");
            assert!(listed.opens(&lookup.clone(), name), "{name}");
        }
        assert!(!listed.opens(&lookup, "file-0"));
        assert_eq!(lookup.reads(), 1);
        let next = listed.settled();
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
        let lookup = listed.settled();
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

    /// A directory with more names than a kept listing holds is read for each lookup, and still
    /// answers each one.
    #[test]
    fn a_directory_beyond_the_kept_names_is_read_for_each_lookup() {
        let names: Vec<String> = (0..=KEPT_NAMES)
            .map(|index| format!("File-{index}"))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let listed = Listed::new(&names);
        let lookup = listed.settled();
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
        let lookup = listed.lookup(changed + Duration::from_secs(1));
        assert!(listed.opens(&lookup, "Actual"));
        assert!(listed.opens(&lookup, "Actual"));
        assert_eq!(lookup.reads(), 2);
    }
}
