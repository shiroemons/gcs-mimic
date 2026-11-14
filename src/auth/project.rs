//! Project IDs and numbers that requests name.

/// A project ID or number a request names: nonempty, at most 256 bytes, without control or
/// whitespace characters. Stored bucket records keep their `project` as `String`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProjectId(String);

/// Text that is not a project ID or number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidProjectId;

impl std::fmt::Display for InvalidProjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("project must be a nonempty identifier of at most 256 bytes")
    }
}

impl std::error::Error for InvalidProjectId {}

/// Shared identifier rules; adapters that only check a value supply their own field names and
/// error messages.
pub(crate) fn valid_project_id(project: &str) -> bool {
    !project.is_empty()
        && project.len() <= 256
        && !project
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

impl ProjectId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectId {
    type Error = InvalidProjectId;

    fn try_from(value: String) -> Result<Self, InvalidProjectId> {
        if valid_project_id(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidProjectId)
        }
    }
}

impl std::str::FromStr for ProjectId {
    type Err = InvalidProjectId;

    fn from_str(value: &str) -> Result<Self, InvalidProjectId> {
        Self::try_from(value.to_owned())
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ProjectId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids_are_nonempty_bounded_and_free_of_spaces_and_controls() {
        for valid in ["p", "my-project", "123456789012", &"x".repeat(256), "日本"] {
            assert_eq!(
                valid
                    .parse::<ProjectId>()
                    .map(|project| project.to_string()),
                Ok(valid.to_owned()),
                "{valid:?}"
            );
        }
        for invalid in [
            "",
            &"x".repeat(257),
            "a b",
            "a\tb",
            "a\u{3000}b",
            "a\nb",
            "a\u{0}b",
            "a\u{7f}b",
        ] {
            assert_eq!(
                invalid.parse::<ProjectId>(),
                Err(InvalidProjectId),
                "{invalid:?}"
            );
        }
        assert_eq!(
            InvalidProjectId.to_string(),
            "project must be a nonempty identifier of at most 256 bytes"
        );
    }
}
