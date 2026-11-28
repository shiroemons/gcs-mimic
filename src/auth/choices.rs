//! The authentication settings' choices and how a profile settles them.

crate::choice::string_enum! {
    pub(crate) enum AuthMode {
        #[default]
        Permissive => "permissive",
        Strict => "strict",
    }
    "auth_mode must be permissive or strict"
}

crate::choice::string_enum! {
    no_default
    /// Whether the built-in [`test_signer`](super::test_signer) identity exists. Its private key is published, so
    /// the default depends on the profile; see [`TestSignerChoice::select`].
    pub(crate) enum TestSigner {
        Enabled => "enabled",
        Disabled => "disabled",
    }
    "test_signer must be enabled or disabled"
}

/// How a profile settles the built-in test signer. Validation selects the rule; only strict
/// mode's default needs the identity document, so loading settles that one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestSignerChoice {
    /// An explicit `test_signer`, or a default that the profile alone decides.
    Fixed(TestSigner),
    /// Strict mode's default: enabled exactly when the identity document declares
    /// `test-signer@gcs-mimic.invalid`.
    WhenDeclared,
}

impl TestSignerChoice {
    /// An explicit setting wins. Otherwise permissive mode, which authorizes nothing by
    /// identity, enables the signer wherever signatures verify; where they are ignored it would
    /// do nothing. Strict mode leaves it to the identity document: anyone can sign with the
    /// published key, so it would turn `allAuthenticatedUsers` grants into public access, and
    /// an entry for its ID says that this local setup asked for it.
    pub(crate) fn select(
        configured: Option<TestSigner>,
        mode: AuthMode,
        signed_urls: SignedUrls,
    ) -> Self {
        match (configured, mode, signed_urls) {
            (Some(signer), _, _) => Self::Fixed(signer),
            (None, AuthMode::Permissive, SignedUrls::Verify) => Self::Fixed(TestSigner::Enabled),
            (None, AuthMode::Permissive, SignedUrls::Ignore) => Self::Fixed(TestSigner::Disabled),
            (None, AuthMode::Strict, SignedUrls::Ignore | SignedUrls::Verify) => Self::WhenDeclared,
        }
    }

    /// The effective value, given whether the identity document declares the signer's ID.
    pub(super) fn settle(self, declares_signer: impl FnOnce() -> bool) -> TestSigner {
        match self {
            Self::Fixed(signer) => signer,
            Self::WhenDeclared if declares_signer() => TestSigner::Enabled,
            Self::WhenDeclared => TestSigner::Disabled,
        }
    }
}

crate::choice::string_enum! {
    no_default
    /// How signed XML requests are treated. Strict mode always verifies them, because their
    /// signer is the caller; permissive mode ignores their credentials unless asked to verify.
    pub(crate) enum SignedUrls {
        Ignore => "ignore",
        Verify => "verify",
    }
    "signed_urls must be ignore or verify"
}

impl SignedUrls {
    /// An explicit setting wins; otherwise strict mode verifies and permissive mode ignores.
    pub(crate) fn resolve(configured: Option<Self>, mode: AuthMode) -> Self {
        configured.unwrap_or(match mode {
            AuthMode::Permissive => Self::Ignore,
            AuthMode::Strict => Self::Verify,
        })
    }
}
