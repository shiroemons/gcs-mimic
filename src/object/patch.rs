//! What a metadata write asks to change, field by field. Object and bucket updates share these
//! request values; the session document keeps its own wire wrappers
//! (`storage::catalog::resumable::options`), so changing these types never changes stored bytes.

use std::collections::BTreeMap;

/// Whether a metadata write merges into (PATCH) or replaces (PUT) the stored resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteMode {
    /// Fields the request does not name keep their values.
    #[default]
    Patch,
    /// Fields the request does not name are reset, as a PUT does.
    Replace,
}

/// A change to one optional field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Patch<T> {
    /// The request does not name the field.
    #[default]
    Keep,
    /// The request names the field as null (or as an empty proto3 string under an update mask).
    Clear,
    /// The request sets the field to this value.
    Set(T),
}

impl<T> Patch<T> {
    /// A field the request names with a value that may be null: `None` clears it.
    pub(crate) fn nullable(value: Option<T>) -> Self {
        value.map_or(Self::Clear, Self::Set)
    }

    /// The value this patch sets; `None` when it keeps or clears the field.
    pub(crate) fn value(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Keep | Self::Clear => None,
        }
    }
}

/// A change to a string map: custom object metadata or bucket labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MapPatch {
    /// The request does not name the map.
    #[default]
    Keep,
    /// The request removes every entry (null, or a JSON map whose values were all dropped).
    Clear,
    /// The named keys change and the others stay: a value stores the key, `None` removes it.
    Merge(BTreeMap<String, Option<String>>),
    /// The map becomes exactly these entries: a `metadata` or `labels` update-mask path, a PUT of
    /// bucket labels, or a configured bucket. A replacement cannot remove a key.
    Replace(BTreeMap<String, String>),
}

impl MapPatch {
    /// Adds one key's change, as a `metadata.KEY` or `labels.KEY` update-mask path or an
    /// `x-goog-meta-*` header does. A kept map starts merging from no entries; so does a cleared
    /// one, which callers reach only for a new object, where nothing remains to clear. A
    /// replacement takes the value or drops the key.
    pub(crate) fn merge_entry(&mut self, key: String, value: Option<String>) {
        match self {
            Self::Merge(entries) => {
                entries.insert(key, value);
            }
            Self::Replace(entries) => match value {
                Some(value) => {
                    entries.insert(key, value);
                }
                None => {
                    entries.remove(&key);
                }
            },
            Self::Keep | Self::Clear => *self = Self::Merge(BTreeMap::from([(key, value)])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(entries: &[(&str, Option<&str>)]) -> MapPatch {
        MapPatch::Merge(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
                .collect(),
        )
    }

    fn replace(entries: &[(&str, &str)]) -> MapPatch {
        MapPatch::Replace(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn a_kept_or_cleared_map_starts_merging_from_the_named_key() {
        for mut patch in [MapPatch::Keep, MapPatch::Clear] {
            patch.merge_entry("key".into(), None);
            assert_eq!(patch, merge(&[("key", None)]));
        }
    }

    #[test]
    fn a_merge_adds_and_overwrites_keys_and_keeps_removals() {
        let mut patch = merge(&[("kept", Some("one")), ("changed", Some("old"))]);
        patch.merge_entry("changed".into(), Some("new".into()));
        patch.merge_entry("removed".into(), None);
        assert_eq!(
            patch,
            merge(&[
                ("kept", Some("one")),
                ("changed", Some("new")),
                ("removed", None),
            ])
        );
    }

    #[test]
    fn a_replacement_takes_values_and_drops_removed_keys() {
        let mut patch = replace(&[("kept", "one"), ("dropped", "two")]);
        patch.merge_entry("added".into(), Some("three".into()));
        patch.merge_entry("dropped".into(), None);
        patch.merge_entry("absent".into(), None);
        assert_eq!(patch, replace(&[("kept", "one"), ("added", "three")]));
    }
}
