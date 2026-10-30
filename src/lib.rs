mod acl;
pub mod auth;
pub mod bounded_file;
pub mod bucket;
pub mod bucket_init;
pub mod clock;
pub mod config;
pub mod config_input;
mod csek;
pub mod diagnostics;
// Linux/macOS descriptor xattr ABIs are isolated behind safe bounded wrappers.
#[expect(
    unsafe_code,
    reason = "descriptor xattr ABIs need FFI; the unsafe calls stay behind bounded safe wrappers"
)]
pub mod fsutil;
mod grpc;
mod hashing;
mod http_conditions;
pub mod import;
pub mod json;
mod match_glob;
pub mod mime;
mod notification;
pub mod object;
pub mod seed;
pub mod server;
pub mod source_tree;
pub mod storage;
pub mod sync;
#[cfg(test)]
mod test_support;
pub mod tls;
pub mod url;
mod xml;
