//! The Rust that `grpc-codegen` generates from the pinned protobuf sources in `src/grpc/proto`:
//! the Storage v2 messages and service trait, the Google API, IAM, type and RPC packages they
//! use, and the retired Storage v1 API. It is a crate of its own so that a change to the server
//! does not recompile it. Regenerate it with `grpc-codegen` instead of editing it.
#![expect(
    clippy::allow_attributes,
    reason = "prost writes `#[allow]` attributes into the generated files, which are not ours \
              to turn into expectations"
)]
#![allow(
    clippy::all,
    rustdoc::broken_intra_doc_links,
    reason = "generated upstream code keeps its own style and documentation links; which of \
              these lints fire varies by target, so an expectation could go unfulfilled"
)]

include!("mod.rs");

/// The retired Storage v1 API. It is generated in a pass of its own, before v2, so the packages
/// both versions use keep the v2 output; that pass writes no module file, so it lives here
/// rather than under `google::storage`.
pub mod storage_v1 {
    include!("google.storage.v1.rs");
}
