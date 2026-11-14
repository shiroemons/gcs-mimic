//! What a metadata write asks to change. Object and bucket updates share these request values;
//! the session document keeps its own wire wrappers (`storage::catalog::resumable::options`), so
//! changing these types never changes stored bytes.

/// Whether a metadata write merges into (PATCH) or replaces (PUT) the stored resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteMode {
    /// Fields the request does not name keep their values.
    #[default]
    Patch,
    /// Fields the request does not name are reset, as a PUT does.
    Replace,
}
