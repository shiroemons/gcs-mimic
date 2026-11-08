//! The authentication profile that one valid configuration selects. Validation and loading both
//! use [`AuthProfile::select`], so the pairing rules live in one place.

use super::{
    Config, ConfigurationReport, Diagnostic, DiagnosticSource, DocumentKind, DocumentSource, Form,
    Setting, Settings,
};
use crate::auth::{
    Auth, AuthMode, SignedUrls, TestSigner, TrustedPrincipalError, signature::SignedHostPort,
};
use std::borrow::Cow;

/// What a profile that verifies signatures needs besides its identity document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verification {
    pub test_signer: TestSigner,
    pub signed_host_port: SignedHostPort,
}

/// Only valid pairings of `auth_mode`, `signed_urls` and the identity document exist as values,
/// so loading a profile needs no checks of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthProfile {
    /// Credentials are ignored and nothing is authorized.
    Permissive,
    /// Signed XML requests verify first, then get permissive access.
    VerifyingPermissive {
        identities: Option<DocumentSource>,
        verification: Verification,
    },
    /// Registered principals and their grants authorize every request.
    Strict {
        identities: DocumentSource,
        verification: Verification,
    },
}

/// A setting that contradicts the profile the others select.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mismatch {
    StrictWithoutIdentities,
    StrictIgnoringSignatures,
    /// The identity document that the setting of this form supplies would go unused.
    IdentitiesIgnored(Form),
    LenientIgnored,
    TestSignerIgnored,
}

/// Either setting can supply the identity document, so both describe one input.
const IDENTITY_SETTINGS: &[Setting] = &[Setting::IdentityFile, Setting::Identities];

impl Mismatch {
    /// The setting whose input the diagnostic names.
    fn reported_at(self) -> Setting {
        match self {
            Self::StrictWithoutIdentities => Setting::AuthMode,
            Self::StrictIgnoringSignatures => Setting::SignedUrls,
            Self::IdentitiesIgnored(form) => DocumentKind::Identities.setting(form),
            Self::LenientIgnored => Setting::SignedHostPort,
            Self::TestSignerIgnored => Setting::TestSigner,
        }
    }

    /// The settings that become invalid, so later checks and loading skip them.
    fn invalidates(self) -> &'static [Setting] {
        match self {
            Self::StrictWithoutIdentities | Self::IdentitiesIgnored(_) => IDENTITY_SETTINGS,
            Self::StrictIgnoringSignatures => &[Setting::SignedUrls],
            Self::LenientIgnored => &[Setting::SignedHostPort],
            Self::TestSignerIgnored => &[Setting::TestSigner],
        }
    }

    /// Settings whose own invalid value already explains the mismatch.
    fn depends_on(self) -> &'static [Setting] {
        match self {
            Self::StrictWithoutIdentities => IDENTITY_SETTINGS,
            Self::StrictIgnoringSignatures => &[Setting::AuthMode, Setting::SignedUrls],
            Self::IdentitiesIgnored(_) => &[
                Setting::AuthMode,
                Setting::SignedUrls,
                Setting::IdentityFile,
                Setting::Identities,
            ],
            Self::LenientIgnored => &[
                Setting::AuthMode,
                Setting::SignedUrls,
                Setting::SignedHostPort,
            ],
            Self::TestSignerIgnored => {
                &[Setting::AuthMode, Setting::SignedUrls, Setting::TestSigner]
            }
        }
    }

    fn fix(self) -> Cow<'static, str> {
        match self {
            Self::StrictWithoutIdentities => {
                "auth_mode=strict requires identity_file or identities; provide --identity-file identities.json or the registry itself with --identities JSON.".into()
            }
            Self::StrictIgnoringSignatures => {
                "signed_urls=ignore requires auth_mode=permissive; strict mode always verifies signatures. Remove the setting or use signed_urls=verify.".into()
            }
            Self::IdentitiesIgnored(form) => format!(
                "{} requires auth_mode=strict, or signed_urls=verify to verify signatures with its public keys; select strict mode, set signed_urls=verify or remove the registry setting.",
                DocumentKind::Identities.setting(form).as_str()
            )
            .into(),
            // Lenient only widens how a signature verifies, so it would claim an effect where
            // nothing is verified.
            Self::LenientIgnored => {
                "signed_host_port=lenient has no effect while signatures are ignored; set signed_urls=verify or auth_mode=strict, or remove the setting.".into()
            }
            // An explicit disabled stays accepted: it is already true where nothing verifies.
            Self::TestSignerIgnored => {
                "test_signer=enabled has no effect while signatures are ignored; set signed_urls=verify or auth_mode=strict, or remove the setting.".into()
            }
        }
    }
}

/// The settings a profile reads; an invalid value among them leaves no profile to load.
const PROFILE_SETTINGS: [Setting; 6] = [
    Setting::AuthMode,
    Setting::IdentityFile,
    Setting::Identities,
    Setting::SignedUrls,
    Setting::TestSigner,
    Setting::SignedHostPort,
];

impl AuthProfile {
    /// The profile the configuration selects, or every setting that contradicts it.
    pub(super) fn select(config: &Config) -> Result<Self, Vec<Mismatch>> {
        let verification = Verification {
            test_signer: config.effective_test_signer(),
            signed_host_port: config.signed_host_port,
        };
        match (
            config.auth_mode,
            config.signed_urls,
            config.identities.as_ref(),
        ) {
            (AuthMode::Strict, Some(SignedUrls::Ignore), identities) => {
                let mut mismatches = vec![Mismatch::StrictIgnoringSignatures];
                if identities.is_none() {
                    mismatches.push(Mismatch::StrictWithoutIdentities);
                }
                Err(mismatches)
            }
            (AuthMode::Strict, None | Some(SignedUrls::Verify), Some(identities)) => {
                Ok(Self::Strict {
                    identities: identities.clone(),
                    verification,
                })
            }
            (AuthMode::Strict, None | Some(SignedUrls::Verify), None) => {
                Err(vec![Mismatch::StrictWithoutIdentities])
            }
            (AuthMode::Permissive, Some(SignedUrls::Verify), identities) => {
                Ok(Self::VerifyingPermissive {
                    identities: identities.cloned(),
                    verification,
                })
            }
            (AuthMode::Permissive, None | Some(SignedUrls::Ignore), identities) => {
                let mismatches: Vec<_> = [
                    identities.map(|source| Mismatch::IdentitiesIgnored(source.form())),
                    (config.signed_host_port == SignedHostPort::Lenient)
                        .then_some(Mismatch::LenientIgnored),
                    (config.test_signer == Some(TestSigner::Enabled))
                        .then_some(Mismatch::TestSignerIgnored),
                ]
                .into_iter()
                .flatten()
                .collect();
                if mismatches.is_empty() {
                    Ok(Self::Permissive)
                } else {
                    Err(mismatches)
                }
            }
        }
    }
}

impl ConfigurationReport {
    /// Rejects mismatched profiles, so a registry is never silently ignored or missing and a
    /// signature setting never claims what the profile does not do.
    pub(super) fn check_profile_pairing(&mut self, settings: &Settings) {
        match AuthProfile::select(&self.config) {
            Ok(profile) if !self.any_invalid_setting(&PROFILE_SETTINGS) => {
                self.auth_profile = Some(profile);
            }
            Ok(_) => {}
            Err(mismatches) => {
                for mismatch in mismatches {
                    if self.any_invalid_setting(mismatch.depends_on()) {
                        continue;
                    }
                    if let Some(input) = settings.get(mismatch.reported_at().as_str()) {
                        self.diagnostics.errors.push(Diagnostic::new(
                            input.source,
                            &input.key,
                            mismatch.fix(),
                        ));
                    }
                    for setting in mismatch.invalidates() {
                        self.invalid.insert(setting.as_str().to_owned());
                    }
                }
            }
        }
    }

    fn any_invalid_setting(&self, settings: &[Setting]) -> bool {
        settings
            .iter()
            .any(|setting| self.invalid.contains(setting.as_str()))
    }

    /// The profile to load, when the authentication settings are valid.
    pub fn auth_profile(&self) -> Option<&AuthProfile> {
        self.auth_profile.as_ref()
    }

    /// Loads the profile that validation selected, reading its identity document once, and
    /// checks the principals that settings name against it. Validation owns the pairing rules,
    /// so only the document's contents and those names can fail here; each failure is recorded
    /// with the setting it concerns. Without a valid profile this is permissive, because
    /// validation has already reported why startup cannot proceed.
    pub fn load_auth(&mut self) -> Auth {
        let Some(profile) = &self.auth_profile else {
            return Auth::permissive();
        };
        // Only a strict registry has principals for a setting to name.
        let registry = match profile {
            AuthProfile::Strict { identities, .. } => Some(identities.form()),
            AuthProfile::Permissive | AuthProfile::VerifyingPermissive { .. } => None,
        };
        match Auth::load(profile) {
            Ok(mut auth) => {
                if let Some(form) = registry {
                    self.resolve_principals(&mut auth, DocumentKind::Identities.setting(form));
                }
                auth
            }
            Err(error) => {
                self.diagnostics.errors.push(Diagnostic::new(
                    DiagnosticSource::Identity,
                    error.setting().as_str(),
                    error.to_string(),
                ));
                Auth::permissive()
            }
        }
    }

    /// Checks the principals that settings name against the loaded registry, which `registry`
    /// supplied, and makes requests without credentials on the trusted listener act as
    /// `trusted_principal`.
    fn resolve_principals(&mut self, auth: &mut Auth, registry: Setting) {
        let registry = registry.as_str();
        if self.valid(&[Setting::AdminPrincipal.as_str()])
            && let (Some(principal), Some(principals)) =
                (&self.config.admin_principal, auth.principals())
            && !principals.iter().any(|identity| identity.id() == principal)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Identity,
                Setting::AdminPrincipal.as_str(),
                format!(
                    "admin_principal {principal:?} is not an id in {registry}; use one of its identities."
                ),
            ));
        }
        if self.valid(&[Setting::TrustedPrincipal.as_str()])
            && let Some(principal) = self.config.trusted_principal.as_deref()
        {
            match auth.trust_principal(principal) {
                Ok(()) => {}
                // Startup already fails then: the setting was rejected for permissive mode.
                Err(TrustedPrincipalError::RequiresStrict) => {}
                Err(TrustedPrincipalError::Unknown) => self.diagnostics.errors.push(Diagnostic::new(
                    DiagnosticSource::Identity,
                    Setting::TrustedPrincipal.as_str(),
                    format!(
                        "trusted_principal {principal:?} is not an id in {registry}; use one of its identities, or the built-in test signer with test_signer=enabled."
                    ),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests;
