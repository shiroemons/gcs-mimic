//! Bucket names typed at once, comma-separated, which then share one set of answers: buckets
//! made for the same purpose are named together instead of answered again one at a time.

use gcs_mimic::bucket_init::generate::{Bucket, GenerateError};
use std::fmt;

/// One bucket name or more, in the order typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Names {
    first: String,
    rest: Vec<String>,
}

impl Names {
    /// Reads `answer`, skipping empty names around commas, and refuses a name that startup would
    /// refuse, a name given twice, and a name that `taken` holds.
    pub(super) fn parse(answer: &str, taken: &[String]) -> Result<Self, NameError> {
        let mut names: Vec<String> = Vec::new();
        for name in answer
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if let Err(error) = Bucket::new(name) {
                return Err(NameError::Invalid {
                    name: name.to_owned(),
                    error,
                });
            }
            if names.iter().any(|seen| seen == name) {
                return Err(NameError::Twice(name.to_owned()));
            }
            if taken.iter().any(|taken| taken == name) {
                return Err(NameError::Taken(name.to_owned()));
            }
            names.push(name.to_owned());
        }
        Self::listed(names).ok_or(NameError::Empty)
    }

    /// `names` together, for a question about all of them, or `None` when there are none.
    pub(super) fn listed(names: impl IntoIterator<Item = String>) -> Option<Self> {
        let mut names = names.into_iter();
        let first = names.next()?;
        Some(Self {
            first,
            rest: names.collect(),
        })
    }

    pub(super) fn first(&self) -> &str {
        &self.first
    }

    pub(super) fn rest(&self) -> &[String] {
        &self.rest
    }

    /// The names as they would be typed, to fill in the question when it asks again.
    pub(super) fn comma_separated(&self) -> String {
        std::iter::once(self.first.as_str())
            .chain(self.rest.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `a`, `a and b`, or `a, b and c`, for a question about every named bucket.
impl fmt::Display for Names {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.first)?;
        let Some((last, middle)) = self.rest.split_last() else {
            return Ok(());
        };
        for name in middle {
            write!(f, ", {name}")?;
        }
        write!(f, " and {last}")
    }
}

/// Why typed bucket names were refused.
#[derive(Debug, thiserror::Error)]
pub(super) enum NameError {
    Empty,
    Invalid {
        name: String,
        #[source]
        error: GenerateError,
    },
    Twice(String),
    Taken(String),
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Enter a bucket name, or several separated by commas."),
            Self::Invalid { name, error } => write!(f, "{name}: {error}"),
            Self::Twice(name) => write!(f, "{name} is named twice."),
            Self::Taken(name) => write!(f, "This document already has a bucket named {name}."),
        }
    }
}

#[cfg(test)]
#[test]
fn invalid_name_keeps_the_rejected_name_and_its_source() {
    let error = Names::parse("uploads_", &[]).unwrap_err();
    let NameError::Invalid { error: cause, .. } = &error else {
        panic!("unexpected {error:?}")
    };
    assert_eq!(
        error.to_string(),
        "uploads_: Bucket names use lower-case letters, digits, '-', '_' and '.', 3 to 63 characters (222 with dots), and start and end with a letter or digit, for example app-uploads."
    );
    let source = std::error::Error::source(&error)
        .unwrap()
        .downcast_ref::<GenerateError>()
        .unwrap();
    assert!(std::ptr::eq(source, cause));
}
