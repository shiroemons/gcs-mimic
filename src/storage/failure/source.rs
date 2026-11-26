//! Classification and source identities for model and storage errors.

use crate::cause::{Layer, Link, MemoryBudget, MemoryExhausted, TypedSource};
use crate::{
    bucket::CatalogError,
    object::ObjectError,
    storage::{admin::AdminError, catalog::RollbackError, import_journal::ResumeError, memory::BlobError},
};
#[cfg(test)]
use crate::cause::{BackendCause, BackendFailure, Source, chain};
#[cfg(test)]
use std::{error::Error, io};

macro_rules! classified_layers {
    ($($error:ty),* $(,)?) => {
        $(impl Layer for $error {
            fn link(&self) -> Link<'_> {
                Link::Typed(self)
            }

            fn next(&self) -> Option<Link<'_>> {
                self.typed_source()
            }
        })*
    };
}

classified_layers!(CatalogError, ObjectError, AdminError, ResumeError, RollbackError);

/// A full RAM store exposes the budget; its other failures expose the original error.
impl Layer for BlobError {
    fn link(&self) -> Link<'_> {
        static BLOBS: MemoryExhausted = MemoryExhausted::new(MemoryBudget::Blobs);
        match self {
            Self::Exhausted => Link::Exhausted(&BLOBS),
            Self::Io(error) => Link::Io(error),
            Self::Size(error) => Link::Leaf(error),
            Self::Allocation(error) => Link::Leaf(error),
            Self::ReadWorker(error) => Link::Leaf(error),
            #[cfg(unix)]
            Self::ShardParentMissing => Link::Leaf(self),
            Self::Missing(_)
            | Self::Poisoned(_)
            | Self::Closed
            | Self::SharedFile
            | Self::ShardNotDirectory => Link::Leaf(self),
        }
    }
}

macro_rules! leaf_sources {
    ($($error:ty),* $(,)?) => {
        $(impl Layer for $error {
            fn link(&self) -> Link<'_> {
                Link::Leaf(self)
            }
        })*
    };
}

leaf_sources!(
    crate::time_format::TimeOutOfRange,
    std::collections::TryReserveError,
    std::num::TryFromIntError,
    std::array::TryFromSliceError,
    std::time::SystemTimeError,
    std::sync::PoisonError<()>,
    std::sync::mpsc::TrySendError<()>,
    getrandom::Error,
    rusqlite::types::FromSqlError,
    serde_json::Error,
    tokio::sync::AcquireError,
    tokio::sync::TryLockError,
    tokio::sync::oneshot::error::RecvError,
    tokio::task::JoinError,
    crate::task::BlockingFailed,
    crate::task::JobPanicked,
    crate::fsutil::FsError,
    crate::acl::AclError,
    crate::auth::InvalidUploadSnapshot,
    crate::auth::PolicyDecodeError,
    crate::auth::signature::VerificationError,
    crate::import::codec::CodecError,
    crate::import::codec::TimestampError,
    crate::notification::NotificationDecodeError,
    crate::object::HashMismatch,
    crate::storage::opaque_id::InvalidOpaqueId,
    crate::storage::admin::NestedFileJob,
    crate::storage::catalog::resumable::InvalidOptions,
    crate::storage::xml_multipart::Error,
);

// A copy checkpoint that tests arm and then abandon.
#[cfg(test)]
leaf_sources!(crate::server::copy::CheckpointError);

#[cfg(test)]
mod tests;
