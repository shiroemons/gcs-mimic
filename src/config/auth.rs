//! The authentication profile that one valid configuration selects. Validation and loading both
//! use [`AuthProfile::select`], so the pairing rules live in one place.

use super::{
    Config, ConfigurationReport, Diagnostic, DiagnosticSource, DocumentKind, DocumentSource, Form,
    Setting, Settings,
};
use crate::auth::{
    Auth, AuthMode, BuiltIn, SignedUrls, TRUSTED_APP_ID, TestSigner, TestSignerChoice,
    TrustedPrincipalError, signature::SignedHostPort, test_signer,
};
use std::{borrow::Cow, collections::BTreeSet};

/// What a profile that verifies signatures needs besides its identity document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verification {
    pub test_signer: TestSignerChoice,
    /// The explicit `signed_host_port`; loading applies the default that the settled test signer
    /// selects, with [`SignedHostPort::resolve`].
    pub signed_host_port: Option<SignedHostPort>,
}

/// Who a request without credentials on the trusted listener acts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustedCaller {
    /// No trusted listener, so every such request is anonymous.
    Anonymous,
    /// The registered identity that `trusted_principal` names; the principal checks after
    /// loading confirm that it is registered.
    Named(String),
    /// The built-in application identity, when `trusted_listen` names no principal.
    BuiltIn,
}

impl TrustedCaller {
    fn select(config: &Config) -> Self {
        match (config.trusted_listen, &config.trusted_principal) {
            (None, _) => Self::Anonymous,
            (Some(_), Some(principal)) => Self::Named(principal.clone()),
            (Some(_), None) => Self::BuiltIn,
        }
    }
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
    /// Registered principals and their grants authorize every request. Without an identity
    /// document only built-in identities exist, which validation allows only where one of them
    /// was asked for.
    Strict {
        identities: Option<DocumentSource>,
        verification: Verification,
        trusted: TrustedCaller,
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
            // An invalid built-in request already explains why none started the registry.
            Self::StrictWithoutIdentities => &[
                Setting::IdentityFile,
                Setting::Identities,
                Setting::TestSigner,
                Setting::TrustedListen,
                Setting::TrustedPrincipal,
            ],
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
            // Without a registry every request would answer 401, so a forgotten document fails
            // here unless a built-in identity was asked for.
            Self::StrictWithoutIdentities => {
                "auth_mode=strict requires identity_file or identities; provide --identity-file identities.json or the registry itself with --identities JSON. To start with built-in identities only, set --trusted-listen without --trusted-principal, or --test-signer enabled.".into()
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
    /// Whether strict mode may start without an identity document: the settings ask for a
    /// built-in identity, the trusted listener's application or an explicitly enabled test
    /// signer, so the server has a caller to authorize.
    fn built_in_only(config: &Config) -> bool {
        TrustedCaller::select(config) == TrustedCaller::BuiltIn
            || config.test_signer == Some(TestSigner::Enabled)
    }

    /// The profile the configuration selects, or every setting that contradicts it.
    pub(super) fn select(config: &Config) -> Result<Self, Vec<Mismatch>> {
        let verification = Verification {
            test_signer: TestSignerChoice::select(
                config.test_signer,
                config.auth_mode,
                config.effective_signed_urls(),
            ),
            signed_host_port: config.signed_host_port,
        };
        match (
            config.auth_mode,
            config.signed_urls,
            config.identities.as_ref(),
        ) {
            (AuthMode::Strict, Some(SignedUrls::Ignore), identities) => {
                let mut mismatches = vec![Mismatch::StrictIgnoringSignatures];
                if identities.is_none() && !Self::built_in_only(config) {
                    mismatches.push(Mismatch::StrictWithoutIdentities);
                }
                Err(mismatches)
            }
            (AuthMode::Strict, None | Some(SignedUrls::Verify), None)
                if !Self::built_in_only(config) =>
            {
                Err(vec![Mismatch::StrictWithoutIdentities])
            }
            (AuthMode::Strict, None | Some(SignedUrls::Verify), identities) => Ok(Self::Strict {
                identities: identities.cloned(),
                verification,
                trusted: TrustedCaller::select(config),
            }),
            (AuthMode::Permissive, Some(SignedUrls::Verify), identities) => {
                Ok(Self::VerifyingPermissive {
                    identities: identities.cloned(),
                    verification,
                })
            }
            (AuthMode::Permissive, None | Some(SignedUrls::Ignore), identities) => {
                let mismatches: Vec<_> = [
                    identities.map(|source| Mismatch::IdentitiesIgnored(source.form())),
                    (config.signed_host_port == Some(SignedHostPort::Lenient))
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
    /// checks the principals that settings name against it. Validation owns the pairing rules
    /// and selects how the test signer is chosen, so only the document's contents and those
    /// names can fail here, and only the document settles strict mode's test signer default;
    /// each failure or warning is recorded with the setting it concerns. Without a valid profile
    /// this is permissive, because validation has already reported why startup cannot proceed.
    pub fn load_auth(&mut self) -> Auth {
        let Some(profile) = &self.auth_profile else {
            return Auth::permissive();
        };
        // Only strict mode has built-in identities with implicit roles.
        let (document, implicit_projects) = match profile {
            AuthProfile::Strict {
                identities,
                verification,
                trusted,
            } => {
                // Only a built-in identity's implicit roles need the buckets document, so nothing
                // else reads it here.
                let projects = match (trusted, verification.test_signer) {
                    (TrustedCaller::BuiltIn, _)
                    | (_, TestSignerChoice::Fixed(TestSigner::Enabled))
                    | (_, TestSignerChoice::WhenDeclared) => self.config.implicit_projects(),
                    (
                        TrustedCaller::Anonymous | TrustedCaller::Named(_),
                        TestSignerChoice::Fixed(TestSigner::Disabled),
                    ) => BTreeSet::new(),
                };
                (identities.as_ref(), projects)
            }
            AuthProfile::VerifyingPermissive { identities, .. } => {
                (identities.as_ref(), BTreeSet::new())
            }
            AuthProfile::Permissive => (None, BTreeSet::new()),
        };
        // The setting that supplied the identity document, which messages name.
        let registry = document.map(|source| DocumentKind::Identities.setting(source.form()));
        match Auth::load(profile, &implicit_projects) {
            Ok(mut auth) => {
                self.warn_strict_test_signer(&auth, registry, &implicit_projects);
                self.resolve_principals(&mut auth, registry);
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

    /// Strict mode has the built-in signer only on request, from `test_signer=enabled` or from
    /// an entry for its ID in `registry`; say what that exposes, at the setting that asked,
    /// including the implicit role in `projects` when no entry replaces it.
    fn warn_strict_test_signer(
        &mut self,
        auth: &Auth,
        registry: Option<Setting>,
        projects: &BTreeSet<String>,
    ) {
        // Permissive mode authorizes nothing by identity, so the published key exposes nothing.
        match (auth.mode(), auth.test_signer()) {
            (AuthMode::Strict, TestSigner::Enabled) => {}
            (AuthMode::Permissive, _) | (AuthMode::Strict, TestSigner::Disabled) => return,
        }
        let id = test_signer::ID;
        let implicit = if auth.declares(id) {
            String::new()
        } else {
            format!(
                " It holds {} in {}, so it can read and write objects there.",
                BuiltIn::TestSigner.role().unwrap_or_default(),
                projects
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let (setting, cause, remedy) = match (self.config.test_signer, registry) {
            (Some(_), _) => (
                Setting::TestSigner,
                "test_signer=enabled in strict mode".to_owned(),
                "",
            ),
            (None, Some(registry)) => (
                registry,
                format!(
                    "{} declares {id}, so strict mode enables the built-in test signer",
                    registry.as_str()
                ),
                ", or set test_signer=disabled to keep the entry without the built-in key",
            ),
            // Without a document nothing declares the signer, so only test_signer enables it.
            (None, None) => return,
        };
        if let Some(input) = self.settings.get(setting.as_str()) {
            self.diagnostics.warnings.push(Diagnostic::new(
                input.source,
                &input.key,
                format!(
                    "{cause}: the private key of {id} is published, so anyone can authenticate as that identity and reach allAuthenticatedUsers grants and anything granted to it.{implicit} Use it only for local tests{remedy}."
                ),
            ));
        }
    }

    /// Checks the principals that settings name against the loaded registry, whose document
    /// `registry` supplied, and makes requests without credentials on the trusted listener act as
    /// `trusted_principal`.
    fn resolve_principals(&mut self, auth: &mut Auth, registry: Option<Setting>) {
        let unknown = match registry {
            Some(registry) => format!(
                "is not an id in {}; use one of its identities",
                registry.as_str()
            ),
            None => {
                "is not a registered identity; declare it in identity_file or identities".to_owned()
            }
        };
        if self.valid(&[Setting::AdminPrincipal.as_str()])
            && let (Some(principal), Some(principals)) =
                (&self.config.admin_principal, auth.principals())
            && !principals.iter().any(|identity| identity.id() == principal)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Identity,
                Setting::AdminPrincipal.as_str(),
                format!("admin_principal {principal:?} {unknown}."),
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
                        "trusted_principal {principal:?} {unknown}, the built-in test signer with test_signer=enabled, or omit trusted_principal to act as the built-in {TRUSTED_APP_ID}."
                    ),
                )),
            }
        }
    }
}

impl Config {
    /// Where the built-in identities hold their implicit roles: the effective `seed_project`
    /// and every project the buckets document names, so that the application can manage the
    /// buckets its own setup declares and nothing else.
    fn implicit_projects(&self) -> BTreeSet<String> {
        let mut projects = self
            .buckets
            .as_ref()
            .map(|source| crate::bucket_init::named_projects(source, self.max_buckets_per_batch))
            .unwrap_or_default();
        projects.insert(self.seed_project.clone());
        projects
    }
}

#[cfg(test)]
mod tests;
