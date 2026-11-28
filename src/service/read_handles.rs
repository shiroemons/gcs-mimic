//! The read handles a gRPC bidi read issues, so a later stream on any connection can resume the
//! same object snapshot. They belong to one server state.

use crate::storage::Finality;
use std::{collections::VecDeque, sync::Mutex, time::SystemTime};

/// Bounds the remembered handles so the table does not grow with the reads a server serves.
const MAX_HANDLES: usize = 1024;

#[derive(Clone, Copy)]
pub(crate) struct HandleToken([u8; 32]);

impl HandleToken {
    /// A token drawn from the operating system's random source.
    pub(crate) fn random() -> Result<Self, getrandom::Error> {
        let mut token = Self([0; 32]);
        getrandom::fill(&mut token.0)?;
        Ok(token)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn matches(self, other: &[u8]) -> bool {
        other.len() == self.0.len()
            && self
                .0
                .iter()
                .zip(other)
                .fold(0, |difference, (a, b)| difference | (a ^ b))
                == 0
    }
}

/// The object snapshot a handle resumes.
#[derive(Clone)]
pub(crate) struct Handle {
    pub(crate) token: HandleToken,
    pub(crate) bucket: String,
    pub(crate) name: String,
    pub(crate) generation: i64,
    pub(crate) size: u64,
    pub(crate) finality: Finality,
    pub(crate) project: Option<String>,
    pub(crate) expires: SystemTime,
}

/// Read snapshots belong to one server state and survive individual gRPC connections.
#[derive(Default)]
pub(crate) struct ReadHandles(Mutex<VecDeque<Handle>>);

impl ReadHandles {
    pub(crate) fn insert(&self, handle: Handle, now: SystemTime) {
        let mut entries = crate::sync::lock(&self.0);
        entries.retain(|entry| entry.expires > now);
        if entries.len() == MAX_HANDLES {
            entries.pop_front();
        }
        entries.push_back(handle);
    }

    pub(crate) fn find(&self, token: &[u8], now: SystemTime) -> Option<Handle> {
        let mut entries = crate::sync::lock(&self.0);
        entries.retain(|entry| entry.expires > now);
        entries
            .iter()
            .find(|entry| entry.token.matches(token))
            .cloned()
    }

    pub(crate) fn remove(&self, token: HandleToken) {
        crate::sync::lock(&self.0).retain(|entry| !entry.token.matches(&token.0));
    }
}

#[cfg(test)]
mod tests;
