//! What a metadata write asks to change, field by field. Object and bucket updates share these
//! request values; the session document keeps its own wire wrappers
//! (`storage::catalog::resumable::options`), so changing these types never changes stored bytes.

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
