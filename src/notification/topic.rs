use super::NotificationError;
use std::fmt;

const PREFIX: &str = "//pubsub.googleapis.com/";

/// A Pub/Sub topic in its full `//pubsub.googleapis.com/projects/{project}/topics/{topic}` form,
/// the only form this server stores, renders and queues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Topic(String);

impl Topic {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Accepts the documented full resource name and the relative `projects/.../topics/...` name
/// that official SDKs send, and keeps only the full form.
impl std::str::FromStr for Topic {
    type Err = NotificationError;

    fn from_str(topic: &str) -> Result<Self, NotificationError> {
        let relative = topic.strip_prefix(PREFIX).unwrap_or(topic);
        let invalid = || NotificationError::InvalidTopic(topic.to_owned());
        let mut parts = relative.split('/');
        let (Some("projects"), Some(project), Some("topics"), Some(name), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(invalid());
        };
        // Project IDs may be domain-scoped (example.com:project); reject only unusable path text.
        if project.is_empty()
            || project.len() > 256
            || !project
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-.:_".contains(&byte))
        {
            return Err(invalid());
        }
        // Pub/Sub resource-name rules for topic IDs.
        if !(3..=255).contains(&name.len())
            || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            || name.starts_with("goog")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.~+%".contains(&byte))
        {
            return Err(invalid());
        }
        Ok(Self(format!("{PREFIX}projects/{project}/topics/{name}")))
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
