//! Probes a root's naming rules with fixed entries in a fixed subdirectory of its workspace.
//!
//! Write-back recovery opens workspace entries only by their 32-digit hexadecimal operation IDs
//! and never lists the workspace, and no name here has that form, so neither the subdirectory nor
//! an entry a crash leaves in it can be taken for an interrupted operation. Each probe starts by
//! unlinking every fixed name rather than listing and removing what it finds, so it never deletes
//! an entry outside its own fixed names, and it never removes the subdirectory. The fixed names
//! include the folded spellings it never creates, because the case and normalization checks take
//! any entry under such a spelling for proof that the filesystem folds it.

use super::{Case, Colon, NameMax, Normalization, Observation, ProbeError, Spelling};
use crate::fsutil::{self, DirectoryNames, DirectoryOpen, Lookup, RegularChild};
use std::{fs::File, io};

/// The workspace subdirectory that holds the probe's entries.
pub(crate) const DIRECTORY: &str = "naming-probe";
/// Created to see whether the filesystem keeps its letter case, then looked up as [`FOLDED`].
const CASED: &str = "Case-Probe";
/// [`CASED`] in lower case. The probe only unlinks and looks it up, and never creates it.
pub(super) const FOLDED: &str = "case-probe";
/// Created to see whether the filesystem keeps a precomposed (NFC) spelling, then looked up as
/// [`COMPOSED_FOLDED`].
const COMPOSED: &str = "nfc-\u{e9}";
/// [`COMPOSED`] decomposed (NFD). The probe only unlinks and looks it up, and never creates it.
pub(super) const COMPOSED_FOLDED: &str = "nfc-e\u{301}";
/// Created to see whether the filesystem keeps a decomposed (NFD) spelling.
const DECOMPOSED: &str = "nfd-e\u{301}";
/// Created to see whether the filesystem stores a colon.
const COLON: &str = "colon:probe";
/// Every name each probe unlinks before it starts: the names it creates, and the folded spellings
/// [`FOLDED`] and [`COMPOSED_FOLDED`], which a leftover or another writer could have put there. On
/// a root that folds case or normalization, a folded spelling and the one it folds onto are one
/// entry; on one that does not, they are two, so both are unlinked.
pub(super) const UNLINKED: [&str; 6] =
    [CASED, FOLDED, COMPOSED, COMPOSED_FOLDED, DECOMPOSED, COLON];
/// The probe directory holds only the probe's own names. The bound keeps a directory that
/// someone filled from turning one activation into an unbounded listing.
const LISTED: usize = DirectoryNames::PAGE;

/// Observes the naming rules of the filesystem that holds `workspace`. `root` only keys the test
/// fault seam and the injected observations.
pub(crate) fn run(root: &File, workspace: &File) -> Result<Observation, ProbeError> {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    if let Some(observed) = injected(root)? {
        return Ok(observed);
    }
    let probe = Probe::open(root, workspace)?;
    for name in UNLINKED {
        probe.remove(name)?;
    }
    let name_max = NameMax::from(fsutil::name_max(&probe.directory)?);
    let (case, cased) = probe.folded(CASED, FOLDED)?;
    let (normalization, composed) = probe.folded(COMPOSED, COMPOSED_FOLDED)?;
    let decomposed = probe.spelling(DECOMPOSED)?;
    let colon = probe.colon()?;
    let spelling = if [cased, composed, decomposed].contains(&Spelling::Rewritten) {
        Spelling::Rewritten
    } else {
        Spelling::Preserved
    };
    Ok(Observation {
        colon,
        case: match case {
            Folded::Apart => Case::Sensitive,
            Folded::Together => Case::Insensitive,
        },
        normalization: match normalization {
            Folded::Apart => Normalization::Sensitive,
            Folded::Together => Normalization::Insensitive,
        },
        spelling,
        name_max,
    })
}

/// Whether a spelling the probe never created opens the entry it created under another one.
enum Folded {
    Apart,
    Together,
}

struct Probe {
    directory: File,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    faults: faults::Paths,
}

impl Probe {
    /// `_root` keys only the test fault seam.
    fn open(_root: &File, workspace: &File) -> io::Result<Self> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        let faults = faults::Paths::new(_root, workspace)?;
        let directory = fsutil::directory_at(
            workspace,
            DIRECTORY.as_ref(),
            DirectoryOpen::CreateIfMissing,
        )?;
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        faults.opened()?;
        Ok(Self {
            directory,
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            faults,
        })
    }

    /// Creates `name`, reports whether the directory lists it with the same spelling and removes
    /// it again. `Err` carries the creation failure itself, for the colon probe to classify.
    fn created(&self, name: &str) -> Result<io::Result<Spelling>, ProbeError> {
        let file = match fsutil::create_regular_at(&self.directory, name.as_ref()) {
            Ok(file) => file,
            Err(error) => return Ok(Err(error)),
        };
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        self.faults.created(name)?;
        let spelling = self.listed(name)?;
        drop(file);
        self.remove(name)?;
        Ok(Ok(spelling))
    }

    fn spelling(&self, name: &str) -> Result<Spelling, ProbeError> {
        Ok(self.created(name)??)
    }

    fn listed(&self, name: &str) -> io::Result<Spelling> {
        let mut names = DirectoryNames::default();
        while let Some(listed) = names.next_name(&self.directory, LISTED)? {
            if listed == name {
                return Ok(Spelling::Preserved);
            }
        }
        Ok(Spelling::Rewritten)
    }

    /// Whether `folded` opens anything once only `created` was created, and whether `created` kept
    /// its spelling. Neither inode numbers nor the spelling check decide: Docker Desktop's shared
    /// folders can report a new inode number for an unchanged file, and each probe unlinks and
    /// creates its names again, so a second probe of the same root could see two numbers for one
    /// file; and on Linux a lookup that knows no probe trusts a directory's casefold flag when it is
    /// clear, which a filesystem that folds names without that flag also reports.
    fn folded(&self, created: &str, folded: &str) -> Result<(Folded, Spelling), ProbeError> {
        let file = fsutil::create_regular_at(&self.directory, created.as_ref())?;
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        self.faults.created(created)?;
        let spelling = self.listed(created)?;
        let folds = match Lookup::unprobed().regular(&self.directory, folded.as_ref())? {
            // The probe unlinked `folded` and never creates it, so opening that spelling at all
            // proves the filesystem folds it onto `created`.
            RegularChild::File(_) | RegularChild::Respelled(_) => Ok(Folded::Together),
            RegularChild::Missing(_) => Ok(Folded::Apart),
            // Whether `folded` reaches `created` is unknown until a later probe.
            RegularChild::Transient(error) => Err(error),
        };
        drop(file);
        self.remove(created)?;
        Ok((folds?, spelling))
    }

    /// A colon the filesystem refuses, or stores under another spelling, is not mapped. That
    /// rewrite concerns only colons, so it does not refuse the root the way a rewritten letter
    /// case or normalization does.
    fn colon(&self) -> Result<Colon, ProbeError> {
        match self.created(COLON)? {
            Ok(Spelling::Preserved) => Ok(Colon::Accepted),
            Ok(Spelling::Rewritten) => Ok(Colon::Rejected),
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::EILSEQ)) => {
                Ok(Colon::Rejected)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Unlinks `name` when it exists; a missing entry is what a finished probe leaves.
    fn remove(&self, name: &str) -> io::Result<()> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        let retained = self.faults.retain(name)?;
        match fsutil::unlink_file_at(&self.directory, name.as_ref()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        self.faults.removed(name, retained)?;
        Ok(())
    }
}

/// The test fault seam: a crash at any boundary of the probe, keyed by the tree root.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod faults {
    use crate::storage::fault::{self, Step};
    use std::{fs::File, io, path::PathBuf};

    pub(super) struct Paths {
        root: PathBuf,
        directory: PathBuf,
        existed: bool,
    }

    impl Paths {
        pub(super) fn new(root: &File, workspace: &File) -> io::Result<Self> {
            let directory = fault::descriptor_path(workspace)?.join(super::DIRECTORY);
            Ok(Self {
                root: fault::descriptor_path(root)?,
                existed: directory.exists(),
                directory,
            })
        }

        /// The workspace is not synced after the directory is created, so a power loss may undo it.
        pub(super) fn opened(&self) -> io::Result<()> {
            if !self.existed {
                fault::dir_created(&self.directory);
            }
            fault::hit_io(&self.root, Step::SyncNamingProbe)
        }

        pub(super) fn created(&self, name: &str) -> io::Result<()> {
            fault::file_created(&self.directory.join(name));
            fault::hit_io(&self.root, Step::SyncNamingProbe)
        }

        pub(super) fn retain(&self, name: &str) -> io::Result<Option<File>> {
            fault::retain_before_unlink(&self.directory.join(name))
        }

        /// `unlink_file_at` syncs the directory after the unlink.
        pub(super) fn removed(&self, name: &str, retained: Option<File>) -> io::Result<()> {
            if let Some(file) = retained {
                fault::unlinked(&self.directory.join(name), &file)?;
            }
            fault::dir_synced(&self.directory);
            fault::hit_io(&self.root, Step::SyncNamingProbe)
        }
    }
}

/// Observations injected for a root by its canonical path, in place of a probe. Tests run in
/// parallel on their own random directories, which a path keeps apart even after one is removed
/// and its inode reused.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
static INJECTED: std::sync::Mutex<Vec<(std::path::PathBuf, Observation)>> =
    std::sync::Mutex::new(Vec::new());

/// Makes every later probe of the root at `tree` return `observed` without touching the disk, so
/// a test can stand in for a filesystem the host does not have. A later injection replaces it.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject(tree: &std::path::Path, observed: Observation) {
    let tree = tree
        .canonicalize()
        .unwrap_or_else(|error| panic!("cannot resolve the injected root: {error}"));
    let mut injected = INJECTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    injected.retain(|(path, _)| *path != tree);
    injected.push((tree, observed));
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn injected(root: &File) -> io::Result<Option<Observation>> {
    let path = crate::storage::fault::descriptor_path(root)?;
    Ok(INJECTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(injected, _)| *injected == path)
        .map(|(_, observed)| *observed))
}
