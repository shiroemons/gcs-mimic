use std::{cmp::Ordering, error::Error, fmt, str::FromStr};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Number(String);

impl FromStr for Number {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(());
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NonZeroNumber(Number);

impl FromStr for NonZeroNumber {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !matches!(value.as_bytes().first(), Some(b'1'..=b'9')) {
            return Err(());
        }
        value.parse().map(Self)
    }
}

impl Ord for Number {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.len().cmp(&other.0.len()).then(self.0.cmp(&other.0))
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Stage {
    Candidate(NonZeroNumber),
    Stable,
}

/// The publication grammar deliberately excludes other SemVer prereleases and build metadata.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Version {
    core: [Number; 3],
    stage: Stage,
}

#[derive(Debug)]
pub struct InvalidVersion(String);

impl fmt::Display for InvalidVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid release version {:?}: releases require MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-rc.N (N >= 1), without leading zeros or build metadata",
            self.0
        )
    }
}

impl Error for InvalidVersion {}

impl FromStr for Version {
    type Err = InvalidVersion;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidVersion(value.to_owned());
        let (core, stage) = if let Some((core, candidate)) = value.split_once("-rc.") {
            let candidate = candidate.parse().map_err(|()| invalid())?;
            (core, Stage::Candidate(candidate))
        } else {
            (value, Stage::Stable)
        };
        let mut parts = core.split('.');
        let core = [
            parts
                .next()
                .ok_or_else(invalid)?
                .parse()
                .map_err(|()| invalid())?,
            parts
                .next()
                .ok_or_else(invalid)?
                .parse()
                .map_err(|()| invalid())?,
            parts
                .next()
                .ok_or_else(invalid)?
                .parse()
                .map_err(|()| invalid())?,
        ];
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(Self { core, stage })
    }
}

impl Version {
    pub(crate) fn is_candidate(&self) -> bool {
        matches!(self.stage, Stage::Candidate(_))
    }

    pub(crate) fn minor_alias(&self) -> String {
        format!("{}.{}", self.core[0].0, self.core[1].0)
    }

    pub(crate) fn same_minor(&self, other: &Self) -> bool {
        self.core[..2] == other.core[..2]
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}",
            self.core[0].0, self.core[1].0, self.core[2].0
        )?;
        match &self.stage {
            Stage::Candidate(number) => write!(f, "-rc.{}", number.0.0),
            Stage::Stable => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
