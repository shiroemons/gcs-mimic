//! Transport-independent authentication profiles, local principals and project-role checks.
//!
//! `permissive` ignores credentials and performs no permission checks; with `signed_urls=verify`
//! it first verifies signed XML requests as strict mode does. `strict` resolves the
//! caller from a local identity registry loaded once at startup and evaluates permissions from
//! the pinned predefined-role snapshot in [`roles`] and scoped bucket policies.
//! Selected object generations also evaluate ACL read grants in catalog transactions.
//! Strict XML query signatures use registered RSA public keys.
//! Nothing here contacts Google or the metadata server.

mod acl_identity;
pub(crate) use acl_identity::validate_local_creator_email;
mod authenticate;
pub use authenticate::Auth;
pub(crate) use authenticate::*;
mod built_in;
pub(crate) use built_in::BuiltIn;
pub(crate) use built_in::TRUSTED_APP_ID;
pub(crate) use built_in::TrustedCaller;
mod choices;
pub(crate) use choices::*;
pub(crate) mod denied;
pub(crate) use denied::Denied;
mod error;
pub(crate) use error::*;
mod identity;
pub(crate) use identity::*;
mod jwt;
mod permission;
pub(crate) use permission::*;
mod policy;
pub(crate) mod policy_input;
mod profile;
pub(crate) use profile::{AuthProfile, Verification};
mod project;
pub(crate) use project::InvalidProjectId;
pub(crate) use project::{ProjectId, valid_project_id, validate_project_id};
mod registry;
pub(crate) use registry::*;
pub(crate) mod roles;
pub(crate) mod signature;
mod signing_keys;
use signing_keys::SigningKeys;
pub(crate) mod test_signer;
pub(crate) mod testable;
pub(crate) use policy::PolicyDecodeError;
pub(crate) use policy::PublicAccess;
pub(crate) use policy::{Binding, Member, MemberRef, Policy};
pub(crate) use roles::Role;

use hyper::{HeaderMap, header::AUTHORIZATION};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let cases: &[(&dyn std::error::Error, &str)] = &[
        (&UnsupportedPermission, "unsupported permission"),
        (&InvalidUploadSnapshot, "invalid upload identity snapshot"),
        (&AuthFailure::LoginRequired, "login required"),
        (&AuthError::Failure(AuthFailure::Forbidden), "forbidden"),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), *expected);
        assert!(error.source().is_none());
    }
    let unavailable = AuthError::Unavailable(crate::cause::BackendFailure::CatalogClosed.into());
    assert_eq!(
        unavailable.to_string(),
        "Authentication verifier unavailable"
    );
    assert!(
        std::error::Error::source(&unavailable)
            .unwrap()
            .is::<crate::cause::BackendCause>()
    );
}
