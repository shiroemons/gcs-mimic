//! The authentication profile that one valid configuration selects. Configuration validation
//! selects it and reads its identity document; loading parses the document's bytes.

use super::{TestSignerChoice, TrustedCaller, signature::SignedHostPort};

/// What a profile that verifies signatures needs besides its identity document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Verification {
    pub(crate) test_signer: TestSignerChoice,
    /// The explicit `signed_host_port`; loading applies the default that the settled test signer
    /// selects, with [`HostPortPolicy::resolve`](super::signature::HostPortPolicy::resolve).
    pub(crate) signed_host_port: Option<SignedHostPort>,
}

/// Only valid pairings of `auth_mode`, `signed_urls` and the identity document exist as values,
/// so loading a profile needs no checks of its own. Outside tests configuration validation is the
/// only constructor, and it is also what keeps strict mode from lacking every caller: that rule
/// spans the identity document, `trusted` and the test signer choice, so a variant of its own
/// would repeat those fields in a form that must agree with them.
///
/// `D` is the identity document: where configuration found it until it is read, then its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AuthProfile<D> {
    /// Credentials are ignored and nothing is authorized.
    Permissive,
    /// Signed XML requests verify first, then get permissive access.
    VerifyingPermissive {
        identities: Option<D>,
        verification: Verification,
    },
    /// Registered principals and their grants authorize every request. Without an identity
    /// document only built-in identities exist, which validation allows only where one of them
    /// was asked for.
    Strict {
        identities: Option<D>,
        verification: Verification,
        trusted: TrustedCaller,
    },
}

impl<D> AuthProfile<D> {
    /// The identity document, which only a profile that verifies signatures carries.
    pub(crate) fn identities(&self) -> Option<&D> {
        match self {
            Self::Permissive => None,
            Self::VerifyingPermissive { identities, .. } | Self::Strict { identities, .. } => {
                identities.as_ref()
            }
        }
    }

    /// The same profile with its identity document converted by `convert`, or the conversion's
    /// failure.
    pub(crate) fn try_map_identities<'a, T, E>(
        &'a self,
        convert: impl FnOnce(&'a D) -> Result<T, E>,
    ) -> Result<AuthProfile<T>, E> {
        Ok(match self {
            Self::Permissive => AuthProfile::Permissive,
            Self::VerifyingPermissive {
                identities,
                verification,
            } => AuthProfile::VerifyingPermissive {
                identities: identities.as_ref().map(convert).transpose()?,
                verification: *verification,
            },
            Self::Strict {
                identities,
                verification,
                trusted,
            } => AuthProfile::Strict {
                identities: identities.as_ref().map(convert).transpose()?,
                verification: *verification,
                trusted: trusted.clone(),
            },
        })
    }
}
