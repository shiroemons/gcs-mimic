//! Native Cloud Storage bucket-name rules.

/// A bucket name under the native Cloud Storage rules. The fake-gcs-server profile admits a
/// superset (`json::validate_bucket_name_for_profile`), so catalog records keep `String`; this type
/// names buckets the local administration tools create and the sync engine maps to directories.
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

/// The native rules.
pub(crate) fn validate(name: &str) -> Result<(), InvalidBucketName> {
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if name.len() < 3
        || name.len() > 222
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
        && parts.iter().all(|part| {
            part.bytes().all(|byte| byte.is_ascii_digit()) && part.parse::<u8>().is_ok()
        })
    {
        return Err(InvalidBucketName);
    }
    Ok(())
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
