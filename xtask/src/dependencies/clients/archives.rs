//! Source archives that a Dockerfile downloads with `ADD --checksum`, for a client without a
//! package manager. The official C++ SDK probe builds the SDK and its libraries from GitHub
//! release archives, so its Dockerfile is both the manifest and the lock: each `ADD` names one
//! archive and its SHA-256, and the snapshot records the same pairs under the release they
//! belong to.

use super::{numeric_release, read};
use crate::dependencies::{
    Result, code_lines, exceptions::Inputs, fetch_github_api, is_digest, json, text,
};
use serde_json::Value;
use std::{fmt, path::Path, str::FromStr};

/// Why the Dockerfile's archive pins, the snapshot or the latest release disagree.
#[derive(Debug, PartialEq, Eq)]
enum ArchiveError {
    /// An `ADD` that downloads without `--checksum`.
    MissingChecksum(String),
    /// An `ADD` other than `--checksum=sha256:<hex> <url> <destination>`.
    UnsupportedAdd(String),
    /// A URL that is neither a GitHub release asset nor a generated tag archive.
    UnsupportedUrl(String),
    DuplicateArchive(String),
    /// An archive from a repository the snapshot records no dependency for.
    UnrecordedRelease(String),
    /// The Dockerfile pins another release than the snapshot's `current`.
    VersionMismatch {
        repository: String,
        dockerfile: String,
        snapshot: String,
    },
    /// An archive of the recorded release that the snapshot does not list.
    UrlMismatch {
        url: String,
        recorded: Vec<String>,
    },
    ChecksumMismatch {
        url: String,
        dockerfile: String,
        snapshot: String,
    },
    /// A recorded archive that the Dockerfile no longer pins.
    MissingFromDockerfile(String),
    /// A release tag that is not dot-separated numbers after an optional `v`.
    UnsupportedTag(String),
    /// GitHub marks a release older than the pin as latest.
    OlderLatest {
        repository: String,
        current: String,
        latest: String,
    },
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const ADD_FORM: &str = "ADD --checksum=sha256:<hex> <url> <destination>";
        match self {
            Self::MissingChecksum(line) => write!(
                f,
                "`{line}` adds a file without --checksum; download archives with `{ADD_FORM}` \
                 and copy build-context files with COPY"
            ),
            Self::UnsupportedAdd(line) => {
                write!(f, "unsupported ADD `{line}`; expected `{ADD_FORM}`")
            }
            Self::UnsupportedUrl(url) => write!(
                f,
                "{url} is neither a GitHub release asset \
                 (https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>) nor a tag \
                 archive (https://github.com/<owner>/<repo>/archive/refs/tags/<tag>.tar.gz)"
            ),
            Self::DuplicateArchive(url) => write!(f, "{url} is pinned twice"),
            Self::UnrecordedRelease(url) => write!(
                f,
                "pins {url}, but the snapshot records no dependency from its repository; \
                 add one to direct_dependencies"
            ),
            Self::VersionMismatch {
                repository,
                dockerfile,
                snapshot,
            } => write!(
                f,
                "pins {repository} {dockerfile}, but the snapshot records {snapshot}; \
                 update the Dockerfile and the snapshot together"
            ),
            Self::UrlMismatch { url, recorded } => write!(
                f,
                "pins {url}, which the snapshot does not record; it records {} for this release",
                recorded.join(", ")
            ),
            Self::ChecksumMismatch {
                url,
                dockerfile,
                snapshot,
            } => write!(
                f,
                "{url} has SHA-256 {dockerfile} in the Dockerfile but {snapshot} in the snapshot"
            ),
            Self::MissingFromDockerfile(url) => {
                write!(
                    f,
                    "the snapshot records {url}, which the Dockerfile does not pin"
                )
            }
            Self::UnsupportedTag(tag) => write!(
                f,
                "release tag {tag} is not a version such as v1.2.3 or 20260817.0"
            ),
            Self::OlderLatest {
                repository,
                current,
                latest,
            } => write!(
                f,
                "GitHub marks {repository} {latest} as the latest release, which is older than \
                 the pinned {current}; check the repository's releases by hand"
            ),
        }
    }
}

impl std::error::Error for ArchiveError {}

/// The GitHub release an archive URL belongs to.
#[derive(Debug, PartialEq, Eq)]
struct Release {
    repository: String,
    tag: String,
}

/// Accepts a release asset, `https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>`,
/// or the archive GitHub generates for a tag,
/// `https://github.com/<owner>/<repo>/archive/refs/tags/<tag>.tar.gz`. Anything else, such as a
/// branch archive or a query string, is rejected rather than guessed at.
impl FromStr for Release {
    type Err = ArchiveError;

    fn from_str(url: &str) -> std::result::Result<Self, Self::Err> {
        let unsupported = || ArchiveError::UnsupportedUrl(url.to_owned());
        let path = url
            .strip_prefix("https://github.com/")
            .ok_or_else(unsupported)?;
        let segments: Vec<&str> = path.split('/').collect();
        let (owner, name, tag) = match segments.as_slice() {
            [owner, name, "releases", "download", tag, asset] if is_name(asset) => {
                (*owner, *name, *tag)
            }
            [owner, name, "archive", "refs", "tags", archive] => (
                *owner,
                *name,
                archive.strip_suffix(".tar.gz").ok_or_else(unsupported)?,
            ),
            _ => return Err(unsupported()),
        };
        if ![owner, name, tag].into_iter().all(is_name) {
            return Err(unsupported());
        }
        Ok(Self {
            repository: format!("{owner}/{name}"),
            tag: tag.to_owned(),
        })
    }
}

/// An owner, repository, tag or asset name as the pinned URLs spell them. The first character
/// must be alphanumeric, so `.` and `..` cannot move the path.
fn is_name(part: &str) -> bool {
    part.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric())
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The numbers a release tag orders by, such as `[4, 5, 0, 0]` for `v4.5.0`.
fn tag_order(tag: &str) -> Option<[u64; 4]> {
    numeric_release(tag.strip_prefix('v').unwrap_or(tag))
}

/// A `sha256:<hex>` digest as Docker's `--checksum` takes it: its digits must be lowercase, or
/// the build rejects it even when the snapshot records the same spelling.
fn is_checksum(digest: &str) -> bool {
    is_digest(digest) && !digest.bytes().any(|b| b.is_ascii_uppercase())
}

/// One `ADD --checksum=sha256:<hex> <url> <destination>` instruction.
#[derive(Debug, PartialEq, Eq)]
struct Pin {
    url: String,
    sha256: String,
    release: Release,
}

/// Reads every `ADD` of a Dockerfile. Each must download one archive and pin its SHA-256, so an
/// archive cannot enter the image unverified or unrecorded.
fn pins(dockerfile: &str) -> std::result::Result<Vec<Pin>, ArchiveError> {
    let mut pins: Vec<Pin> = Vec::new();
    for line in code_lines(dockerfile) {
        let mut words = line.split_whitespace();
        if !words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("ADD"))
        {
            continue;
        }
        let pin = add_pin(&line, &words.collect::<Vec<_>>())?;
        if pins.iter().any(|known| known.url == pin.url) {
            return Err(ArchiveError::DuplicateArchive(pin.url));
        }
        pins.push(pin);
    }
    Ok(pins)
}

fn add_pin(line: &str, arguments: &[&str]) -> std::result::Result<Pin, ArchiveError> {
    let instruction = || line.split_whitespace().collect::<Vec<_>>().join(" ");
    let (flags, operands): (Vec<&str>, Vec<&str>) = arguments
        .iter()
        .copied()
        .partition(|word| word.starts_with("--"));
    let checksums: Vec<&str> = flags
        .iter()
        .filter_map(|flag| flag.strip_prefix("--checksum="))
        .collect();
    let checksum = match checksums.as_slice() {
        [] => return Err(ArchiveError::MissingChecksum(instruction())),
        [checksum] if flags.len() == 1 && is_checksum(checksum) => *checksum,
        _ => return Err(ArchiveError::UnsupportedAdd(instruction())),
    };
    let (Some(sha256), [url, _destination]) =
        (checksum.strip_prefix("sha256:"), operands.as_slice())
    else {
        return Err(ArchiveError::UnsupportedAdd(instruction()));
    };
    Ok(Pin {
        release: url.parse()?,
        url: (*url).to_owned(),
        sha256: sha256.to_owned(),
    })
}

/// A recorded archive: its URL and lowercase hexadecimal SHA-256.
struct Archive<'a> {
    url: &'a str,
    sha256: &'a str,
}

/// A `direct_dependencies` record: one upstream release, named by its tag in `current`, and every
/// archive the Dockerfile pins from it. Both CMake binary archives are one dependency.
struct Dependency<'a> {
    current: &'a str,
    repository: &'a str,
    archives: Vec<Archive<'a>>,
}

/// Reads the snapshot records, each of which must name a license and only archives of its own
/// release, so the offline comparison can trust the repository and tag of a record.
fn dependencies(scope: &Value) -> Result<Vec<Dependency<'_>>> {
    let records = scope["direct_dependencies"]
        .as_array()
        .filter(|records| !records.is_empty())
        .ok_or("missing source archive snapshot")?;
    let mut dependencies: Vec<Dependency<'_>> = Vec::new();
    for record in records {
        let name = text(record, "name")?;
        text(record, "license")?;
        let current = text(record, "current")?;
        let repository = text(record, "repository")?;
        if tag_order(current).is_none() {
            return Err(ArchiveError::UnsupportedTag(current.to_owned()).to_string());
        }
        if dependencies.iter().any(|d| d.repository == repository) {
            return Err(format!("source archive snapshot repeats {repository}"));
        }
        let mut archives = Vec::new();
        for archive in record["archives"]
            .as_array()
            .filter(|archives| !archives.is_empty())
            .ok_or_else(|| format!("missing source archives: {name}"))?
        {
            let url = text(archive, "url")?;
            let sha256 = text(archive, "sha256")?;
            let release: Release = url
                .parse()
                .map_err(|error: ArchiveError| error.to_string())?;
            if release.repository != repository || release.tag != current {
                return Err(format!(
                    "source archive snapshot: {url} is not from {repository} {current}"
                ));
            }
            if !is_checksum(&format!("sha256:{sha256}")) {
                return Err(format!(
                    "source archive snapshot: invalid SHA-256 for {url}"
                ));
            }
            archives.push(Archive { url, sha256 });
        }
        dependencies.push(Dependency {
            current,
            repository,
            archives,
        });
    }
    Ok(dependencies)
}

/// The Dockerfile and the snapshot must name the same archives with the same checksums, each
/// from the release its dependency records.
fn verify(pins: &[Pin], dependencies: &[Dependency<'_>]) -> std::result::Result<(), ArchiveError> {
    for pin in pins {
        let Some(dependency) = dependencies
            .iter()
            .find(|d| d.repository == pin.release.repository)
        else {
            return Err(ArchiveError::UnrecordedRelease(pin.url.clone()));
        };
        if dependency.current != pin.release.tag {
            return Err(ArchiveError::VersionMismatch {
                repository: dependency.repository.to_owned(),
                dockerfile: pin.release.tag.clone(),
                snapshot: dependency.current.to_owned(),
            });
        }
        let Some(recorded) = dependency.archives.iter().find(|a| a.url == pin.url) else {
            return Err(ArchiveError::UrlMismatch {
                url: pin.url.clone(),
                recorded: dependency
                    .archives
                    .iter()
                    .map(|a| a.url.to_owned())
                    .collect(),
            });
        };
        if recorded.sha256 != pin.sha256 {
            return Err(ArchiveError::ChecksumMismatch {
                url: pin.url.clone(),
                dockerfile: pin.sha256.clone(),
                snapshot: recorded.sha256.to_owned(),
            });
        }
    }
    let unpinned = dependencies
        .iter()
        .flat_map(|d| &d.archives)
        .find(|archive| !pins.iter().any(|pin| pin.url == archive.url));
    match unpinned {
        Some(archive) => Err(ArchiveError::MissingFromDockerfile(archive.url.to_owned())),
        None => Ok(()),
    }
}

/// The compiler image the snapshot records must be one the Dockerfile builds from, and its
/// version the one the image tag names. The image digest and its freshness are checked with the
/// other Docker inputs.
fn verify_toolchain(dockerfile: &str, toolchain: &Value) -> Result<()> {
    let image = text(toolchain, "image")?;
    let version = text(toolchain, "version")?;
    let tag = image
        .split_once(':')
        .map(|(_, tag)| tag)
        .ok_or_else(|| format!("toolchain image without a tag: {image}"))?;
    if tag.split('-').next() != Some(version) {
        return Err(format!(
            "toolchain version {version} is not the version of {image}"
        ));
    }
    let pinned = format!("{image}@sha256:");
    let builds_from = code_lines(dockerfile).iter().any(|line| {
        let mut words = line.split_whitespace();
        words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("FROM"))
            && words
                .find(|word| !word.starts_with("--"))
                .is_some_and(|word| word.starts_with(&pinned))
    });
    if !builds_from {
        return Err(format!(
            "the Dockerfile does not build FROM the digest-pinned toolchain image {image}"
        ));
    }
    Ok(())
}

/// The tag of a repository's `releases/latest` answer. GitHub never returns a draft or a
/// prerelease there; the flags are still required, so a changed answer cannot pass for stable.
fn latest_release_tag(release: &Value) -> Result<&str> {
    if release["draft"] != false || release["prerelease"] != false {
        return Err("GitHub latest release is not a published stable release".into());
    }
    text(release, "tag_name")
}

/// Prints and returns whether the latest release updates the pin. GitHub marks the most recently
/// published release latest, so a patch for an older release line can hold the mark; that is an
/// error, because a newer release could exist without the mark and CURRENT would be false.
fn compare_release(
    inputs: &mut Inputs<'_>,
    repository: &str,
    current: &str,
    latest: &str,
) -> std::result::Result<bool, ArchiveError> {
    let order = |tag: &str| tag_order(tag).ok_or_else(|| ArchiveError::UnsupportedTag(tag.into()));
    if order(latest)? < order(current)? {
        return Err(ArchiveError::OlderLatest {
            repository: repository.to_owned(),
            current: current.to_owned(),
            latest: latest.to_owned(),
        });
    }
    Ok(inputs.compare(&format!("source-archive:{repository}"), current, latest))
}

/// Offline, the Dockerfile's `ADD` pins and its compiler image must match the snapshot; online,
/// each recorded release is compared with the latest GitHub release of its repository.
pub(super) fn check(root: &Path, scope: &Value, inputs: &mut Inputs<'_>) -> Result<bool> {
    let manifest = text(scope, "manifest")?;
    let dockerfile = read(root, manifest)?;
    let dependencies = dependencies(scope)?;
    pins(&dockerfile)
        .and_then(|pins| verify(&pins, &dependencies))
        .map_err(|error| format!("{manifest}: {error}"))?;
    verify_toolchain(&dockerfile, &scope["toolchain"])
        .map_err(|error| format!("{manifest}: {error}"))?;
    let mut drift = false;
    for dependency in &dependencies {
        let release = json(&fetch_github_api(&format!(
            "https://api.github.com/repos/{}/releases/latest",
            dependency.repository
        ))?)?;
        let latest = latest_release_tag(&release)?;
        drift |= compare_release(inputs, dependency.repository, dependency.current, latest)
            .map_err(|error| error.to_string())?;
    }
    Ok(drift)
}

#[cfg(test)]
mod tests;
