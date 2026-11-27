mod acl;
pub mod app;
mod auth;
mod base64;
mod bounded_body;
mod bounded_file;
mod bucket;
mod bucket_init;
mod cause;
mod choice;
mod clock;
mod config;
mod config_input;
mod csek;
mod decimal;
mod diagnostics;
mod error;
mod etag;
mod fd_limit;
// The descriptor and xattr FFI calls carry their `unsafe_code` expectations on the individual
// wrapper functions in this module, so new unsafe code elsewhere in it still fails the lint.
mod fsutil;
mod grpc;
mod hashing;
mod hex;
mod http_conditions;
mod http_header;
mod import;
mod input;
mod json;
mod match_glob;
mod memory_limit;
mod metadata_error;
mod mime;
mod multipart;
mod net;
mod notification;
mod object;
mod page_token;
mod percent;
mod project;
mod seed;
mod serde_util;
mod server;
mod service;
mod source_tree;
mod storage;
mod sync;
mod task;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "Unit tests use a subset of the fixtures shared with the integration tests"
)]
mod test_support;
#[doc(hidden)]
pub mod testing;
mod time_format;
mod time_limit;
mod tls;
mod url;
mod xml;

/// The product version from the package manifest. `--version` and the ready event report this
/// one string.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
