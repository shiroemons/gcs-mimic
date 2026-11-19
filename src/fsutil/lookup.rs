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

use super::{Child, DirectoryOpen, RegularChild, imp, stat_identity};
use std::{ffi::OsStr, fs::File, io};

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

/// Opens children under the naming rules a probe found for the root they lie below.
#[derive(Clone, Debug)]
pub(crate) struct Lookup {
    rules: Rules,
}

impl Lookup {
    /// Lookups that know nothing about the filesystem: each directory's own answer decides.
    pub(crate) fn unprobed() -> Self {
        Self {
            rules: Rules::Unprobed,
        }
    }

    /// Lookups below the root on `device` whose filesystem matches names as `matching` says.
    pub(crate) fn probed(device: u64, matching: Matching) -> Self {
        Self {
            rules: match matching {
                Matching::Exact => Rules::Exact { device },
                Matching::Folding => Rules::Folding,
            },
        }
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
        let known = |lookup: Lookup| lookup.known(&directory).unwrap();
        assert_eq!(known(Lookup::probed(device, Matching::Exact)), Known::Exact);
        assert_eq!(
            known(Lookup::probed(other, Matching::Exact)),
            Known::Nothing
        );
        assert_eq!(
            known(Lookup::probed(other, Matching::Folding)),
            Known::Folding
        );
        assert_eq!(known(Lookup::unprobed()), Known::Nothing);
    }
}
