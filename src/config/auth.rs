//! The authentication profile that one valid configuration selects. Validation and loading both
//! use [`AuthProfile::select`], so the pairing rules live in one place.

use super::{Config, ConfigurationReport, Diagnostic, Setting, Settings};
use crate::auth::{AuthMode, SignedUrls, TestSigner, signature::SignedHostPort};
use std::path::PathBuf;

/// What a profile that verifies signatures needs besides its identity file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verification {
    pub test_signer: TestSigner,
    pub signed_host_port: SignedHostPort,
}

/// Only valid pairings of `auth_mode`, `signed_urls` and `identity_file` exist as values, so
/// loading a profile needs no checks of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthProfile {
    /// Credentials are ignored and nothing is authorized.
    Permissive,
    /// Signed XML requests verify first, then get permissive access.
    VerifyingPermissive {
        identity_file: Option<PathBuf>,
        verification: Verification,
    },
    /// Registered principals and their grants authorize every request.
    Strict {
        identity_file: PathBuf,
        verification: Verification,
    },
}

/// A setting that contradicts the profile the others select.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mismatch {
    StrictWithoutIdentityFile,
    StrictIgnoringSignatures,
    IdentityFileIgnored,
    LenientIgnored,
}

impl Mismatch {
    /// The setting whose input the diagnostic names.
    fn reported_at(self) -> Setting {
        match self {
            Self::StrictWithoutIdentityFile => Setting::AuthMode,
            Self::StrictIgnoringSignatures => Setting::SignedUrls,
            Self::IdentityFileIgnored => Setting::IdentityFile,
            Self::LenientIgnored => Setting::SignedHostPort,
        }
    }

    /// The setting that becomes invalid, so later checks and loading skip it.
    fn invalidates(self) -> Setting {
        match self {
            Self::StrictWithoutIdentityFile => Setting::IdentityFile,
            Self::StrictIgnoringSignatures | Self::IdentityFileIgnored | Self::LenientIgnored => {
                self.reported_at()
            }
        }
    }

    /// Settings whose own invalid value already explains the mismatch.
    fn depends_on(self) -> &'static [Setting] {
        match self {
            Self::StrictWithoutIdentityFile => &[Setting::IdentityFile],
            Self::StrictIgnoringSignatures | Self::IdentityFileIgnored => {
                &[Setting::AuthMode, Setting::SignedUrls]
            }
            Self::LenientIgnored => &[
                Setting::AuthMode,
                Setting::SignedUrls,
                Setting::SignedHostPort,
            ],
        }
    }

    fn fix(self) -> &'static str {
        match self {
            Self::StrictWithoutIdentityFile => {
                "auth_mode=strict requires identity_file; provide --identity-file identities.json."
            }
            Self::StrictIgnoringSignatures => {
                "signed_urls=ignore requires auth_mode=permissive; strict mode always verifies signatures. Remove the setting or use signed_urls=verify."
            }
            Self::IdentityFileIgnored => {
                "identity_file requires auth_mode=strict, or signed_urls=verify to verify signatures with its public keys; select strict mode, set signed_urls=verify or remove the registry setting."
            }
            // Lenient only widens how a signature verifies, so it would claim an effect where
            // nothing is verified.
            Self::LenientIgnored => {
                "signed_host_port=lenient has no effect while signatures are ignored; set signed_urls=verify or auth_mode=strict, or remove the setting."
            }
        }
    }
}

/// The settings a profile reads; an invalid value among them leaves no profile to load.
const PROFILE_SETTINGS: [Setting; 5] = [
    Setting::AuthMode,
    Setting::IdentityFile,
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
            config.identity_file.as_ref(),
        ) {
            (AuthMode::Strict, Some(SignedUrls::Ignore), identity_file) => {
                let mut mismatches = vec![Mismatch::StrictIgnoringSignatures];
                if identity_file.is_none() {
                    mismatches.push(Mismatch::StrictWithoutIdentityFile);
                }
                Err(mismatches)
            }
            (AuthMode::Strict, None | Some(SignedUrls::Verify), Some(identity_file)) => {
                Ok(Self::Strict {
                    identity_file: identity_file.clone(),
                    verification,
                })
            }
            (AuthMode::Strict, None | Some(SignedUrls::Verify), None) => {
                Err(vec![Mismatch::StrictWithoutIdentityFile])
            }
            (AuthMode::Permissive, Some(SignedUrls::Verify), identity_file) => {
                Ok(Self::VerifyingPermissive {
                    identity_file: identity_file.cloned(),
                    verification,
                })
            }
            (AuthMode::Permissive, None | Some(SignedUrls::Ignore), identity_file) => {
                let mismatches: Vec<_> = [
                    identity_file
                        .is_some()
                        .then_some(Mismatch::IdentityFileIgnored),
                    (config.signed_host_port == SignedHostPort::Lenient)
                        .then_some(Mismatch::LenientIgnored),
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
                    self.invalid
                        .insert(mismatch.invalidates().as_str().to_owned());
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
}
