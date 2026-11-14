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

    /// `projects/{project}/topics/{topic}`, the path the Pub/Sub REST API addresses.
    pub(crate) fn relative(&self) -> &str {
        // Parsing builds the full form, so the prefix is always there.
        self.0.strip_prefix(PREFIX).unwrap_or(&self.0)
    }

    /// A topic this server wrote, which must already be in the full form.
    pub(crate) fn from_canonical(stored: &str) -> Option<Self> {
        stored
            .parse::<Self>()
            .ok()
            .filter(|topic| topic.0 == stored)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_full_topics_come_back_from_storage() {
        let full = "//pubsub.googleapis.com/projects/p/topics/topic";
        let topic = Topic::from_canonical(full).unwrap();
        assert_eq!(topic.as_str(), full);
        assert_eq!(topic.relative(), "projects/p/topics/topic");
        // The relative form is accepted from clients but never stored, so a row holding it is
        // damaged, as is one that names no topic at all.
        for stored in [
            "projects/p/topics/topic",
            "//pubsub.googleapis.com/projects/p/topics/ab",
            "//pubsub.googleapis.com/projects/p/subscriptions/topic",
            "",
        ] {
            assert_eq!(Topic::from_canonical(stored), None, "{stored}");
        }
    }
}
