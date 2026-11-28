//! The CORS fallback logs one server state has written, so each bucket is logged once.

use std::{collections::BTreeSet, sync::Mutex};

/// Bounds the remembered bucket names so the warning memory does not grow with the catalog.
const WARNED_BUCKET_LIMIT: usize = 64;

/// Remembers one CORS fallback log per bucket and State, up to 64 buckets: the unconfigured-CORS
/// warning where the fallback is off, or the note where the permissive default allowed a request.
/// The effective fallback is fixed for a process, so only one of them can ever use the set.
#[derive(Default)]
pub(crate) struct CorsWarnings(Mutex<BTreeSet<String>>);

impl CorsWarnings {
    /// Records `bucket` and says whether this is its first logged fallback.
    pub(crate) fn claim(&self, bucket: &str) -> bool {
        let mut warned = crate::sync::lock(&self.0);
        warned.len() < WARNED_BUCKET_LIMIT && warned.insert(bucket.to_owned())
    }

    /// The buckets logged so far.
    #[cfg(test)]
    pub(crate) fn buckets(&self) -> BTreeSet<String> {
        crate::sync::lock(&self.0).clone()
    }
}

#[cfg(test)]
mod tests;
