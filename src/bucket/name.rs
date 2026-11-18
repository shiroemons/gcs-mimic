//! Native Cloud Storage bucket-name rules.

use crate::config::CompatProfile;

/// A bucket name under the native Cloud Storage rules. The fake-gcs-server profile admits a
/// superset ([`validate_for_profile`]), so catalog records keep `String`; this type names
/// buckets the local administration tools create and the sync engine maps to directories.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct BucketName(String);

/// A name outside the native bucket-name rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidBucketName;

impl std::fmt::Display for InvalidBucketName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Invalid bucket name")
    }
}

impl std::error::Error for InvalidBucketName {}

/// The longest native bucket name: dotted names may reach 222 bytes.
pub(crate) const MAX_BUCKET_NAME_BYTES: usize = 222;

/// The native rules.
pub(crate) fn validate(name: &str) -> Result<(), InvalidBucketName> {
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if name.len() < 3
        || name.len() > MAX_BUCKET_NAME_BYTES
        || (!name.contains('.') && name.len() > 63)
        || !name
            .bytes()
            .all(|byte| alphanumeric(byte) || matches!(byte, b'-' | b'_' | b'.'))
        || !name.bytes().next().is_some_and(alphanumeric)
        || !name.bytes().last().is_some_and(alphanumeric)
        || name
            .split('.')
            .any(|component| component.is_empty() || component.len() > 63)
        || name.starts_with("goog")
        || name.replace('0', "o").contains("google")
    {
        return Err(InvalidBucketName);
    }
    let parts: Vec<_> = name.split('.').collect();
    if parts.len() == 4
        && parts
            .iter()
            .all(|part| crate::decimal::Grammar::Digits.parse::<u8>(part).is_some())
    {
        return Err(InvalidBucketName);
    }
    Ok(())
}

/// A requested bucket name that the server's compatibility profile does not admit. The two
/// profiles word the refusal differently, and the JSON API also gives each its own reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProfileNameError {
    /// Outside the native rules, which the native profile applies.
    Native(InvalidBucketName),
    /// Outside the looser rules of the fake-gcs-server profile.
    FakeGcsServer,
}

impl std::fmt::Display for ProfileNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(error) => error.fmt(formatter),
            Self::FakeGcsServer => formatter.write_str("invalid bucket name"),
        }
    }
}

impl std::error::Error for ProfileNameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            Self::FakeGcsServer => None,
        }
    }
}

/// A bucket name a request may name under `profile`: the native rules, or fake-gcs-server's
/// superset of 2 to 666 lowercase letters, digits, dots, hyphens and underscores that starts and
/// ends with a letter or digit.
pub(crate) fn validate_for_profile(
    name: &str,
    profile: CompatProfile,
) -> Result<(), ProfileNameError> {
    if profile == CompatProfile::Native {
        return validate(name).map_err(ProfileNameError::Native);
    }
    let edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if (2..=666).contains(&name.len())
        && name.bytes().next().is_some_and(edge)
        && name.bytes().last().is_some_and(edge)
        && name
            .bytes()
            .all(|byte| edge(byte) || b"._-".contains(&byte))
    {
        Ok(())
    } else {
        Err(ProfileNameError::FakeGcsServer)
    }
}

impl BucketName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BucketName {
    type Error = InvalidBucketName;

    fn try_from(value: String) -> Result<Self, InvalidBucketName> {
        validate(&value)?;
        Ok(Self(value))
    }
}

impl TryFrom<&str> for BucketName {
    type Error = InvalidBucketName;

    fn try_from(value: &str) -> Result<Self, InvalidBucketName> {
        validate(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl std::str::FromStr for BucketName {
    type Err = InvalidBucketName;

    fn from_str(value: &str) -> Result<Self, InvalidBucketName> {
        Self::try_from(value)
    }
}

impl std::fmt::Display for BucketName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for BucketName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
