use super::{ObjectError, custom_time_key, invalid_custom_time, unix_nanos::UnixNanos};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// A custom time preserves the spelling supplied by the caller, with a cached instant for
/// comparisons and gRPC rendering.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CustomTime {
    text: String,
    #[serde(skip)]
    instant: Option<UnixNanos>,
}

impl CustomTime {
    /// Keeps rejected text so requests and old stored rows still fail at the same checks.
    /// Before publishing a new value, check it through metadata validation or `instant()`.
    pub(crate) fn from_text(text: String) -> Self {
        let instant = custom_time_key(&text).ok();
        Self { text, instant }
    }

    pub(crate) fn parse(text: String) -> Result<Self, ObjectError> {
        let time = Self::from_text(text);
        time.instant()?;
        Ok(time)
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn instant(&self) -> Result<UnixNanos, ObjectError> {
        self.instant.ok_or_else(invalid_custom_time)
    }
}

impl<'de> Deserialize<'de> for CustomTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from_text)
    }
}

impl fmt::Debug for CustomTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, formatter)
    }
}

// Existing fixtures include both valid input and corrupt stored text.
#[cfg(test)]
impl From<&str> for CustomTime {
    fn from(text: &str) -> Self {
        Self::from_text(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata_error::MetadataError;

    #[test]
    fn stored_strings_keep_their_spelling_and_defer_invalid_instant_errors() {
        for (text, valid) in [("1969-12-31t23:59:59.123456789z", true), ("invalid", false)] {
            let stored = serde_json::to_string(text).unwrap();
            let time: CustomTime = serde_json::from_str(&stored).unwrap();
            assert_eq!(time.as_str(), text);
            assert_eq!(serde_json::to_string(&time).unwrap(), stored);
            if valid {
                assert_eq!(
                    time.instant().unwrap(),
                    UnixNanos::new(-1, 123_456_789).unwrap()
                );
            } else {
                assert!(matches!(
                    time.instant(),
                    Err(ObjectError::Metadata(MetadataError::CustomTime))
                ));
            }
        }
    }
}
