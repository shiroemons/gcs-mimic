mod acl;
mod api_error;
pub mod auth;
mod base64;
mod bounded_body;
pub mod bounded_file;
pub mod bucket;
pub mod bucket_init;
pub mod clock;
pub mod config;
pub mod config_input;
mod csek;
mod decimal;
pub mod diagnostics;
mod error;
mod etag;
pub mod fd_limit;
// The descriptor and xattr FFI calls carry their `unsafe_code` expectations on the individual
// wrapper functions in this module, so new unsafe code elsewhere in it still fails the lint.
pub mod fsutil;
mod grpc;
mod hashing;
mod hex;
mod http_conditions;
pub mod import;
mod input;
pub mod json;
mod match_glob;
mod memory_limit;
mod metadata_error;
pub mod mime;
mod multipart;
pub mod net;
mod notification;
pub mod object;
mod page_token;
mod percent;
mod project;
pub mod seed;
mod serde_util;
pub mod server;
mod service;
pub mod source_tree;
pub mod storage;
mod task;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "Unit tests use a subset of the fixtures shared with the integration tests"
)]
mod test_support;
mod time_format;
pub mod time_limit;
pub mod tls;
pub mod url;
mod xml;

/// The product version from the package manifest. `--version` and the ready event report this
/// one string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use api_error::ApiError;
