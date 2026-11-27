//! A bucket's answers as a person reads them before the document is written: the questions ask
//! about goals such as browser uploads, so the summary says what the entry will do rather than
//! repeat its JSON members.

use super::{Binding, Bucket, Cors, PUBLIC_READ_ROLE};
use std::fmt;

/// The bucket's name, then one indented line per thing its answers set, or a note that it keeps
/// the default settings.
impl fmt::Display for Bucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.name)?;
        let mut lines = self.summary_lines().peekable();
        if lines.peek().is_none() {
            return writeln!(f, "  default settings");
        }
        for line in lines {
            writeln!(f, "  {line}")?;
        }
        Ok(())
    }
}

impl Bucket {
    /// One line in words per thing the answers set, without the name, for a summary that adds
    /// lines of its own, such as who reads the bucket; none for a bucket with default settings.
    pub(crate) fn summary_lines(&self) -> impl Iterator<Item = String> + '_ {
        let settings = [
            self.cors.as_ref().map(Cors::to_string),
            self.public_read
                .then(|| "public read: anyone can read objects".to_owned()),
            self.versioning
                .map(|_| "versioning: overwrites and deletes keep the old object".to_owned()),
            self.soft_delete_policy
                .map(|policy| match policy.retention_duration_seconds {
                    0 => "soft delete: off".to_owned(),
                    seconds => format!("soft delete: keeps deleted objects for {}", Span(seconds)),
                }),
            (!self.labels.is_empty()).then(|| {
                let labels: Vec<_> = self
                    .labels
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                format!("labels: {}", labels.join(", "))
            }),
            self.iam_configuration.map(|configuration| {
                format!(
                    "public access prevention: {}",
                    configuration.public_access_prevention.as_str()
                )
            }),
            self.default_event_based_hold
                .then(|| "event-based hold: new objects start held".to_owned()),
            self.location
                .as_ref()
                .map(|location| format!("location: {location}")),
            self.storage_class
                .as_ref()
                .map(|class| format!("storage class: {class}")),
            self.project
                .as_ref()
                .map(|project| format!("project: {project}")),
        ];
        // A written policy replaces the default one, whose legacy bindings name projectOwner,
        // projectEditor and projectViewer; no local principal belongs to those sets, so the
        // replacement changes nobody's access here and the summary does not mention it.
        let bindings = self
            .granted()
            .map(|Binding { role, members }| format!("IAM: {role} for {}", members.join(", ")));
        settings.into_iter().flatten().chain(bindings)
    }

    /// The bindings other than the one public read added, which its own line already shows.
    fn granted(&self) -> impl Iterator<Item = &Binding> {
        let bindings = self
            .iam_policy
            .as_ref()
            .map_or(&[][..], |policy| policy.bindings.as_slice());
        let public = self
            .public_read
            .then(|| bindings.iter().position(Binding::grants_public_read))
            .flatten();
        bindings
            .iter()
            .enumerate()
            .filter(move |(index, _)| Some(*index) != public)
            .map(|(_, binding)| binding)
    }
}

impl Binding {
    fn grants_public_read(&self) -> bool {
        self.role == PUBLIC_READ_ROLE && self.members == ["allUsers"]
    }
}

/// `GET, HEAD from http://localhost:3000; headers Content-Type; max age 1 hour`.
impl fmt::Display for Cors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CORS: {} from {}",
            self.method.join(", "),
            self.origin.join(", ")
        )?;
        if !self.response_header.is_empty() {
            write!(f, "; headers {}", self.response_header.join(", "))?;
        }
        if let Some(seconds) = self.max_age_seconds {
            write!(f, "; max age {}", Span(seconds))?;
        }
        Ok(())
    }
}

/// A number of seconds in the largest whole unit, such as `7 days` or `90 seconds`.
struct Span(u64);

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(seconds) = *self;
        let (count, unit) = [(86_400, "day"), (3600, "hour"), (60, "minute")]
            .into_iter()
            .find(|(size, _)| seconds >= *size && seconds % size == 0)
            .map_or((seconds, "second"), |(size, unit)| (seconds / size, unit));
        let plural = if count == 1 { "" } else { "s" };
        write!(f, "{count} {unit}{plural}")
    }
}

#[cfg(test)]
mod tests;
