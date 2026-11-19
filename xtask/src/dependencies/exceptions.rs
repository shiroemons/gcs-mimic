//! Recorded, dated deferrals of an input's update. The snapshot's `exceptions` are parsed once,
//! before any network access, and every input the freshness check reports goes through
//! [`Report`], which decides whether an exception still defers it or has gone stale.

use crate::{
    date::{Date, InvalidDate},
    named,
};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    error::Error,
    fmt, io,
    path::{Component, Path},
};

named! {
    /// The fields of an exception record. Every one is required, and no other is accepted.
    pub(super) enum Field {
        Input => "input",
        Manifests => "manifests",
        Current => "current",
        Latest => "latest",
        Reference => "reference",
        Evidence => "evidence",
        Reason => "reason",
        Impact => "impact",
        Owner => "owner",
        NextAction => "next_action",
        ReviewBy => "review_by",
    }
}

/// What an exception names: the label the check prints for an input and the manifest that
/// declares the input. A label alone is ambiguous: each Java manifest reports its own
/// `Temurin toolchain`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct InputKey {
    label: String,
    manifest: String,
}

impl InputKey {
    fn new(label: &str, manifest: &str) -> Self {
        Self {
            label: label.to_owned(),
            manifest: manifest.to_owned(),
        }
    }
}

impl fmt::Display for InputKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} in {}", self.label, self.manifest)
    }
}

/// What one exception records for an input.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Recorded {
    /// The record's position in `exceptions`, which messages name.
    entry: usize,
    current: String,
    latest: String,
    review_by: Date,
}

impl Recorded {
    /// The lines that say why this exception no longer defers an input that pins `current` while
    /// `latest` is available on `today`. Empty while the exception still holds: it passes through
    /// its review date and fails from the day after.
    fn lapses(&self, key: &InputKey, today: Date, current: &str, latest: &str) -> Vec<String> {
        let entry = self.entry;
        let mut lapses = Vec::new();
        if current != self.current {
            lapses.push(format!(
                "EXCEPTION_OUTDATED: {key}: the pin is {current}, but exceptions[{entry}] records \
                 current={}; record the new pin in the exception or delete it",
                self.current
            ));
        }
        if latest != self.latest {
            lapses.push(format!(
                "EXCEPTION_OUTDATED: {key}: the latest release is {latest}, but exceptions[{entry}] \
                 records latest={}; re-test {latest}, then adopt it or record it in the exception",
                self.latest
            ));
        }
        if today > self.review_by {
            lapses.push(format!(
                "EXCEPTION_EXPIRED: {key}: exceptions[{entry}] had review_by={}; adopt {latest}, \
                 or re-test it and record a new review_by",
                self.review_by
            ));
        }
        lapses
    }
}

/// The snapshot's exceptions, at most one per input.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Exceptions(BTreeMap<InputKey, Recorded>);

impl Exceptions {
    /// Parses and checks the snapshot's `exceptions` list; evidence paths resolve against `root`.
    pub(super) fn parse(root: &Path, exceptions: &Value) -> Result<Self, ExceptionError> {
        let records = exceptions.as_array().ok_or(ExceptionError::NotAList)?;
        let mut by_input = BTreeMap::<InputKey, Recorded>::new();
        for (index, value) in records.iter().enumerate() {
            let record = Record::parse(root, index, value)?;
            for manifest in &record.manifests {
                match by_input.entry(InputKey::new(record.input, manifest)) {
                    Entry::Occupied(first) => {
                        return Err(ExceptionError::Duplicate {
                            key: first.key().clone(),
                            first: first.get().entry,
                            second: index,
                        });
                    }
                    Entry::Vacant(slot) => {
                        slot.insert(record.recorded.clone());
                    }
                }
            }
        }
        Ok(Self(by_input))
    }
}

/// One record of `exceptions`, before each of its manifests becomes a key.
struct Record<'a> {
    input: &'a str,
    manifests: Vec<&'a str>,
    recorded: Recorded,
}

impl<'a> Record<'a> {
    fn parse(root: &Path, index: usize, value: &'a Value) -> Result<Self, ExceptionError> {
        let values = value
            .as_object()
            .ok_or(ExceptionError::NotAnObject { index })?;
        let fields = Fields {
            values,
            entry: EntryName {
                index,
                input: values
                    .get(Field::Input.as_str())
                    .and_then(Value::as_str)
                    .filter(|input| !input.trim().is_empty())
                    .map(str::to_owned),
            },
        };
        if let Some(unknown) = values.keys().find(|name| name.parse::<Field>().is_err()) {
            return Err(ExceptionError::Unknown {
                entry: fields.entry(),
                field: unknown.clone(),
            });
        }
        let input = fields.text(Field::Input)?;
        let manifests = fields.manifests()?;
        let current = fields.text(Field::Current)?;
        let latest = fields.text(Field::Latest)?;
        for field in [
            Field::Reference,
            Field::Reason,
            Field::Impact,
            Field::Owner,
            Field::NextAction,
        ] {
            fields.text(field)?;
        }
        fields.evidence(root)?;
        let review_by =
            fields
                .text(Field::ReviewBy)?
                .parse()
                .map_err(|source| ExceptionError::ReviewBy {
                    entry: fields.entry(),
                    source,
                })?;
        Ok(Self {
            input,
            manifests,
            recorded: Recorded {
                entry: index,
                current: current.to_owned(),
                latest: latest.to_owned(),
                review_by,
            },
        })
    }
}

/// An exception record's fields, with the name its errors give the record.
struct Fields<'a> {
    values: &'a Map<String, Value>,
    entry: EntryName,
}

impl<'a> Fields<'a> {
    fn entry(&self) -> EntryName {
        self.entry.clone()
    }

    fn value(&self, field: Field) -> Result<&'a Value, ExceptionError> {
        self.values
            .get(field.as_str())
            .ok_or_else(|| ExceptionError::Missing {
                entry: self.entry(),
                field,
            })
    }

    /// A required string with more than whitespace in it.
    fn text(&self, field: Field) -> Result<&'a str, ExceptionError> {
        let text = self
            .value(field)?
            .as_str()
            .ok_or_else(|| ExceptionError::NotText {
                entry: self.entry(),
                field,
            })?;
        if text.trim().is_empty() {
            return Err(ExceptionError::Empty {
                entry: self.entry(),
                field,
            });
        }
        Ok(text)
    }

    /// The non-empty list of distinct manifests the exception applies to.
    fn manifests(&self) -> Result<Vec<&'a str>, ExceptionError> {
        let field = Field::Manifests;
        let not_text = || ExceptionError::NotText {
            entry: self.entry(),
            field,
        };
        let empty = || ExceptionError::Empty {
            entry: self.entry(),
            field,
        };
        let list = self.value(field)?.as_array().ok_or_else(not_text)?;
        if list.is_empty() {
            return Err(empty());
        }
        let mut manifests = Vec::new();
        for item in list {
            let manifest = item.as_str().ok_or_else(not_text)?;
            if manifest.trim().is_empty() {
                return Err(empty());
            }
            if manifests.contains(&manifest) {
                return Err(ExceptionError::Repeated {
                    entry: self.entry(),
                    manifest: manifest.to_owned(),
                });
            }
            manifests.push(manifest);
        }
        Ok(manifests)
    }

    /// The evidence must be a file in the repository; a `#fragment` names a section of it.
    fn evidence(&self, root: &Path) -> Result<(), ExceptionError> {
        let evidence = self.text(Field::Evidence)?;
        let path = evidence.split_once('#').map_or(evidence, |(path, _)| path);
        let relative = Path::new(path);
        let path = path.to_owned();
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(ExceptionError::EvidenceOutside {
                entry: self.entry(),
                path,
            });
        }
        match std::fs::metadata(root.join(relative)) {
            Ok(metadata) if metadata.is_file() => Ok(()),
            Err(source) if source.kind() != io::ErrorKind::NotFound => {
                Err(ExceptionError::UnreadableEvidence {
                    entry: self.entry(),
                    path,
                    source,
                })
            }
            Ok(_) | Err(_) => Err(ExceptionError::MissingEvidence {
                entry: self.entry(),
                path,
            }),
        }
    }
}

/// Names an exception record in messages: its position, and its input when it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EntryName {
    index: usize,
    input: Option<String>,
}

impl fmt::Display for EntryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exceptions[{}]", self.index)?;
        match &self.input {
            Some(input) => write!(f, " ({input})"),
            None => Ok(()),
        }
    }
}

/// Why the snapshot's `exceptions` cannot be used. Each names the record and, where one is at
/// fault, the field.
#[derive(Debug)]
pub(super) enum ExceptionError {
    /// The snapshot has no `exceptions` list.
    NotAList,
    NotAnObject {
        index: usize,
    },
    Unknown {
        entry: EntryName,
        field: String,
    },
    Missing {
        entry: EntryName,
        field: Field,
    },
    /// A value of the wrong JSON type.
    NotText {
        entry: EntryName,
        field: Field,
    },
    /// A string that is empty or only whitespace, or an empty `manifests` list.
    Empty {
        entry: EntryName,
        field: Field,
    },
    /// A manifest that one record's `manifests` lists twice.
    Repeated {
        entry: EntryName,
        manifest: String,
    },
    ReviewBy {
        entry: EntryName,
        source: InvalidDate,
    },
    /// An evidence path that is absolute or leaves the repository.
    EvidenceOutside {
        entry: EntryName,
        path: String,
    },
    MissingEvidence {
        entry: EntryName,
        path: String,
    },
    UnreadableEvidence {
        entry: EntryName,
        path: String,
        source: io::Error,
    },
    /// Two records that name the same input.
    Duplicate {
        key: InputKey,
        first: usize,
        second: usize,
    },
}

impl fmt::Display for ExceptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAList => f.write_str("the snapshot's `exceptions` must be a list"),
            Self::NotAnObject { index } => write!(f, "exceptions[{index}] must be an object"),
            Self::Unknown { entry, field } => {
                let fields: Vec<_> = Field::ALL.iter().map(|field| field.as_str()).collect();
                write!(
                    f,
                    "{entry}: unknown field `{field}`; an exception has exactly {}",
                    fields.join(", ")
                )
            }
            Self::Missing { entry, field } => write!(f, "{entry}: missing field `{field}`"),
            Self::NotText {
                entry,
                field: Field::Manifests,
            } => write!(f, "{entry}: `manifests` must be a list of strings"),
            Self::NotText { entry, field } => write!(f, "{entry}: `{field}` must be a string"),
            Self::Empty { entry, field } => write!(f, "{entry}: `{field}` is empty"),
            Self::Repeated { entry, manifest } => {
                write!(f, "{entry}: `manifests` lists {manifest} twice")
            }
            Self::ReviewBy { entry, .. } => {
                write!(f, "{entry}: `review_by` must be a YYYY-MM-DD date")
            }
            Self::EvidenceOutside { entry, path } => write!(
                f,
                "{entry}: `evidence` {path:?} is not a repository-relative path"
            ),
            Self::MissingEvidence { entry, path } => {
                write!(f, "{entry}: `evidence` file {path} does not exist")
            }
            Self::UnreadableEvidence { entry, path, .. } => {
                write!(f, "{entry}: `evidence` file {path} cannot be read")
            }
            Self::Duplicate { key, first, second } => write!(
                f,
                "exceptions[{first}] and exceptions[{second}] both record {key}; keep one \
                 exception per input"
            ),
        }
    }
}

impl Error for ExceptionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReviewBy { source, .. } => Some(source),
            Self::UnreadableEvidence { source, .. } => Some(source),
            Self::NotAList
            | Self::NotAnObject { .. }
            | Self::Unknown { .. }
            | Self::Missing { .. }
            | Self::NotText { .. }
            | Self::Empty { .. }
            | Self::Repeated { .. }
            | Self::EvidenceOutside { .. }
            | Self::MissingEvidence { .. }
            | Self::Duplicate { .. } => None,
        }
    }
}

/// Whether a run lets an exception defer an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Deferrals {
    /// The full check: an exception defers its input's update while it still describes the input
    /// and `today` is no later than its review date.
    Honored { today: Date },
    /// The release gate: a temporary pin deviates from the freshness target, so an update to a
    /// release input fails even when an exception names it.
    Refused,
}

/// What the check found for an input, which also decides how its line reads.
#[derive(Clone, Copy, Debug)]
enum Found<'a> {
    /// The pin is the latest release.
    Latest(&'a str),
    /// A moving image tag whose `declared` platforms all match the pin.
    LatestOn { declared: &'a str },
    /// A newer release than the pin; for a moving image tag, `on` the listed platforms.
    Update {
        current: &'a str,
        latest: &'a str,
        on: Option<&'a str>,
    },
}

impl Found<'_> {
    fn is_update(self) -> bool {
        matches!(self, Self::Update { .. })
    }

    /// The input's line when no exception applies.
    fn line(self, label: &str) -> String {
        match self {
            Self::Latest(version) => format!("{label}: CURRENT current={version} latest={version}"),
            Self::LatestOn { declared } => {
                format!("{label}: CURRENT on declared platforms {declared}")
            }
            Self::Update {
                current,
                latest,
                on,
            } => format!(
                "{label}: UPDATE_AVAILABLE{} current={current} latest={latest}",
                platforms(on)
            ),
        }
    }
}

fn platforms(on: Option<&str>) -> String {
    on.map_or_else(String::new, |platforms| format!(" on {platforms}"))
}

/// Lines to print, and whether they fail the check.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) lines: Vec<String>,
    pub(super) fails: bool,
}

/// Reports every input of one freshness check, applying the recorded exceptions.
#[derive(Debug)]
pub(super) struct Report {
    exceptions: Exceptions,
    deferrals: Deferrals,
    /// Inputs that an exception names and the check reported; an exception for any other input
    /// is stale.
    observed: BTreeSet<InputKey>,
    /// The review date of each deferred input.
    deferred: Vec<Date>,
}

impl Report {
    pub(super) fn new(exceptions: Exceptions, deferrals: Deferrals) -> Self {
        Self {
            exceptions,
            deferrals,
            observed: BTreeSet::new(),
            deferred: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn without_exceptions() -> Self {
        Self::new(Exceptions::default(), Deferrals::Refused)
    }

    /// Reports the inputs that `manifest` declares.
    pub(super) fn inputs_of<'a>(&'a mut self, manifest: &'a str) -> Inputs<'a> {
        Inputs {
            report: self,
            manifest,
        }
    }

    fn record(&mut self, key: InputKey, found: Found<'_>) -> Outcome {
        let as_found = found.line(&key.label);
        let Some(recorded) = self.exceptions.0.get(&key) else {
            return Outcome {
                lines: vec![as_found],
                fails: found.is_update(),
            };
        };
        match (self.deferrals, found) {
            (Deferrals::Refused, Found::Latest(_) | Found::LatestOn { .. }) => Outcome {
                lines: vec![as_found],
                fails: false,
            },
            (Deferrals::Refused, Found::Update { latest, .. }) => Outcome {
                lines: vec![
                    as_found,
                    format!(
                        "EXCEPTION_REFUSED: {key}: an exception cannot defer a release input under \
                         --scope release; adopt {latest} before releasing"
                    ),
                ],
                fails: true,
            },
            (Deferrals::Honored { .. }, Found::Latest(_) | Found::LatestOn { .. }) => {
                let stale = format!(
                    "EXCEPTION_STALE: {key} reports CURRENT; remove it from exceptions[{}]",
                    recorded.entry
                );
                self.observed.insert(key);
                Outcome {
                    lines: vec![as_found, stale],
                    fails: true,
                }
            }
            (
                Deferrals::Honored { today },
                Found::Update {
                    current,
                    latest,
                    on,
                },
            ) => {
                let lapses = recorded.lapses(&key, today, current, latest);
                let outcome = if lapses.is_empty() {
                    self.deferred.push(recorded.review_by);
                    let deferred = format!(
                        "{}: DEFERRED{} current={current} latest={latest} review_by={} manifest={}",
                        key.label,
                        platforms(on),
                        recorded.review_by,
                        key.manifest
                    );
                    Outcome {
                        lines: vec![deferred],
                        fails: false,
                    }
                } else {
                    Outcome {
                        lines: [as_found].into_iter().chain(lapses).collect(),
                        fails: true,
                    }
                };
                self.observed.insert(key);
                outcome
            }
        }
    }

    /// Ends the run. In the full check an exception that matched no reported input is stale; the
    /// release gate skips development-only inputs, so there it matches nothing by design. A run
    /// that deferred an input says so last, because its dependencies are not all current.
    pub(super) fn finish(self) -> Outcome {
        let stale: Vec<String> = match self.deferrals {
            Deferrals::Honored { .. } => self
                .exceptions
                .0
                .iter()
                .filter(|(key, _)| !self.observed.contains(key))
                .map(|(key, recorded)| {
                    format!(
                        "EXCEPTION_STALE: {key} matches no checked input; correct the input and \
                         manifests of exceptions[{}], or remove it",
                        recorded.entry
                    )
                })
                .collect(),
            Deferrals::Refused => Vec::new(),
        };
        let fails = !stale.is_empty();
        let mut lines = stale;
        if let Some(earliest) = self.deferred.iter().min() {
            lines.push(format!(
                "DEFERRED: {} updates deferred by recorded exceptions, earliest review_by \
                 {earliest}; dependencies are not all current",
                self.deferred.len()
            ));
        }
        Outcome { lines, fails }
    }
}

/// Reports the inputs one manifest declares; see [`Report::inputs_of`].
pub(super) struct Inputs<'a> {
    report: &'a mut Report,
    manifest: &'a str,
}

impl Inputs<'_> {
    /// Reports `label` as CURRENT when `current` is the `latest` release and as an update
    /// otherwise, and returns whether it fails the check.
    pub(super) fn compare(&mut self, label: &str, current: &str, latest: &str) -> bool {
        let found = if current == latest {
            Found::Latest(current)
        } else {
            Found::Update {
                current,
                latest,
                on: None,
            }
        };
        self.print(label, found)
    }

    /// A moving image tag that matches its pin on every `declared` platform.
    pub(super) fn latest_on(&mut self, label: &str, declared: &str) -> bool {
        self.print(label, Found::LatestOn { declared })
    }

    /// A moving image tag whose `drifted` declared platforms differ from the pin.
    pub(super) fn update_on(
        &mut self,
        label: &str,
        drifted: &str,
        current: &str,
        latest: &str,
    ) -> bool {
        self.print(
            label,
            Found::Update {
                current,
                latest,
                on: Some(drifted),
            },
        )
    }

    fn print(&mut self, label: &str, found: Found<'_>) -> bool {
        let outcome = self
            .report
            .record(InputKey::new(label, self.manifest), found);
        for line in &outcome.lines {
            println!("{line}");
        }
        outcome.fails
    }
}

#[cfg(test)]
mod tests;
