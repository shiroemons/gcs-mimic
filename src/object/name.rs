//! Flat-namespace object-name rules shared by every transport and import path.

/// Which flat-namespace rule a name breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidObjectName {
    Empty,
    TooLong,
    LineBreak,
    DotSegment,
    AcmeChallenge,
}

impl std::fmt::Display for InvalidObjectName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "object name is empty",
            Self::TooLong => "object name exceeds 1024 bytes",
            Self::LineBreak => "object name contains a carriage return or line feed",
            Self::DotSegment => "object name is . or ..",
            Self::AcmeChallenge => "object name starts with .well-known/acme-challenge/",
        })
    }
}

impl std::error::Error for InvalidObjectName {}

/// An object name that satisfies the flat-namespace rules of [`validate`]. Catalog records keep
/// `String`: older catalogs may hold names a transport accepted before these rules were shared.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct ObjectName(String);

impl ObjectName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ObjectName {
    type Error = InvalidObjectName;

    fn try_from(value: String) -> Result<Self, InvalidObjectName> {
        validate(&value)?;
        Ok(Self(value))
    }
}

impl TryFrom<&str> for ObjectName {
    type Error = InvalidObjectName;

    fn try_from(value: &str) -> Result<Self, InvalidObjectName> {
        validate(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl std::str::FromStr for ObjectName {
    type Err = InvalidObjectName;

    fn from_str(value: &str) -> Result<Self, InvalidObjectName> {
        Self::try_from(value)
    }
}

impl std::fmt::Display for ObjectName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ObjectName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

pub(crate) const MAX_OBJECT_NAME_BYTES: usize = 1024;

pub(crate) fn validate(name: &str) -> Result<(), InvalidObjectName> {
    if name.is_empty() {
        return Err(InvalidObjectName::Empty);
    }
    if name.len() > MAX_OBJECT_NAME_BYTES {
        return Err(InvalidObjectName::TooLong);
    }
    if name.contains(['\r', '\n']) {
        return Err(InvalidObjectName::LineBreak);
    }
    if matches!(name, "." | "..") {
        return Err(InvalidObjectName::DotSegment);
    }
    if name.starts_with(".well-known/acme-challenge/") {
        return Err(InvalidObjectName::AcmeChallenge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_namespace_boundaries_and_reserved_names() {
        for name in [
            "...",
            ".well-known/acme-challenge",
            "a\tb",
            &"x".repeat(1024),
            &"é".repeat(512),
        ] {
            assert!(validate(name).is_ok(), "{name:?}");
        }
        for (name, error) in [
            ("", InvalidObjectName::Empty),
            (".", InvalidObjectName::DotSegment),
            ("..", InvalidObjectName::DotSegment),
            ("a\rb", InvalidObjectName::LineBreak),
            ("a\nb", InvalidObjectName::LineBreak),
            (
                ".well-known/acme-challenge/x",
                InvalidObjectName::AcmeChallenge,
            ),
            (&"x".repeat(1025), InvalidObjectName::TooLong),
        ] {
            assert_eq!(validate(name), Err(error));
        }
    }
}
