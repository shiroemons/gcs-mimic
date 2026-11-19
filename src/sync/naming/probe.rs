//! Probes a root's naming rules with fixed entries in a fixed subdirectory of its workspace.
//!
//! Write-back recovery opens workspace entries only by their 32-digit hexadecimal operation IDs
//! and never lists the workspace, and no name here has that form, so neither the subdirectory nor
//! an entry a crash leaves in it can be taken for an interrupted operation. Each probe starts by
//! unlinking every fixed name rather than listing and removing what it finds, so it never deletes
//! an entry it did not create, and it never removes the subdirectory.

use super::{Case, Colon, NameMax, Observation, ProbeError, Spelling};
use crate::fsutil::{self, DirectoryNames, DirectoryOpen};
use std::{fs::File, io};

/// The workspace subdirectory that holds the probe's entries.
pub(crate) const DIRECTORY: &str = "naming-probe";
/// Created to see whether the filesystem keeps its letter case, then looked up as [`FOLDED`].
const CASED: &str = "Case-Probe";
/// [`CASED`] in lower case. The probe only looks it up and never creates it.
pub(super) const FOLDED: &str = "case-probe";
/// Created to see whether the filesystem keeps a precomposed (NFC) spelling.
const COMPOSED: &str = "nfc-\u{e9}";
/// Created to see whether the filesystem keeps a decomposed (NFD) spelling.
const DECOMPOSED: &str = "nfd-e\u{301}";
/// Created to see whether the filesystem stores a colon.
const COLON: &str = "colon:probe";
/// Every name the probe creates; each probe unlinks them all before it starts.
pub(super) const CREATED: [&str; 4] = [CASED, COMPOSED, DECOMPOSED, COLON];
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
    for name in CREATED {
        probe.remove(name)?;
    }
    let name_max = NameMax::from(fsutil::name_max(&probe.directory)?);
    let (case, cased) = probe.case()?;
    let composed = probe.spelling(COMPOSED)?;
    let decomposed = probe.spelling(DECOMPOSED)?;
    let colon = probe.colon()?;
    let spelling = if [cased, composed, decomposed].contains(&Spelling::Rewritten) {
        Spelling::Rewritten
    } else {
        Spelling::Preserved
    };
    Ok(Observation {
        colon,
        case,
        spelling,
        name_max,
    })
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

    /// Whether [`FOLDED`] opens the file created as [`CASED`], and whether that one kept its
    /// spelling.
    fn case(&self) -> Result<(Case, Spelling), ProbeError> {
        let cased = fsutil::create_regular_at(&self.directory, CASED.as_ref())?;
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        self.faults.created(CASED)?;
        let spelling = self.listed(CASED)?;
        let case = match fsutil::open_regular_at(&self.directory, FOLDED.as_ref()) {
            Ok(folded)
                if fsutil::stat_identity(&folded)?.node()
                    == fsutil::stat_identity(&cased)?.node() =>
            {
                Case::Insensitive
            }
            Ok(_) => Case::Sensitive,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Case::Sensitive,
            Err(error) => return Err(error.into()),
        };
        drop(cased);
        self.remove(CASED)?;
        Ok((case, spelling))
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
