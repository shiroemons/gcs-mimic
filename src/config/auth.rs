//! The authentication profile that one valid configuration selects, and the identity document it
//! loads. Validation and loading both use [`select_profile`], so the pairing rules live in one
//! place; loading reads the document here and hands its bytes to `auth`.

use super::{
    ConfigDraft, ConfigurationReport, Diagnostic, DiagnosticSource, DocumentKind, DocumentSource,
    Form, Setting, Settings, Validation,
};
use crate::auth::{
    Auth, AuthMode, AuthProfile, BuiltIn, RegistryError, SignedUrls, TRUSTED_APP_ID, TestSigner,
    TestSignerChoice, TrustedCaller, Verification, signature::SignedHostPort, test_signer,
};
use std::{borrow::Cow, collections::BTreeSet, fmt};

/// The caller that the `trusted_listen` and `trusted_principal` settings select.
fn trusted_caller(config: &ConfigDraft) -> TrustedCaller {
    match (config.trusted_listen, &config.trusted_principal) {
        (None, _) => TrustedCaller::Anonymous,
        (Some(_), Some(principal)) => TrustedCaller::Named(principal.clone()),
        (Some(_), None) => TrustedCaller::BuiltIn,
    }
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

/// Whether strict mode may start without an identity document: the settings ask for a built-in
/// identity, the trusted listener's application or an explicitly enabled test signer, so the
/// server has a caller to authorize.
fn built_in_only(config: &ConfigDraft) -> bool {
    trusted_caller(config) == TrustedCaller::BuiltIn
        || config.test_signer == Some(TestSigner::Enabled)
}

/// The profile the configuration selects, or every setting that contradicts it. Outside tests
/// only this chooses an [`AuthProfile`] variant; reading the document keeps the variant and its
/// other fields.
fn select_profile(config: &ConfigDraft) -> Result<AuthProfile<DocumentSource>, Vec<Mismatch>> {
    let verification = Verification {
        test_signer: TestSignerChoice::select(
            config.test_signer,
            config.auth_mode,
            config.resolved_signed_urls(),
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
            if identities.is_none() && !built_in_only(config) {
                mismatches.push(Mismatch::StrictWithoutIdentities);
            }
            Err(mismatches)
        }
        (AuthMode::Strict, None | Some(SignedUrls::Verify), None) if !built_in_only(config) => {
            Err(vec![Mismatch::StrictWithoutIdentities])
        }
        (AuthMode::Strict, None | Some(SignedUrls::Verify), identities) => {
            Ok(AuthProfile::Strict {
                identities: identities.cloned(),
                verification,
                trusted: trusted_caller(config),
            })
        }
        (AuthMode::Permissive, Some(SignedUrls::Verify), identities) => {
            Ok(AuthProfile::VerifyingPermissive {
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
                Ok(AuthProfile::Permissive)
            } else {
                Err(mismatches)
            }
        }
    }
}

/// The identity document is trusted and read once at startup, and every lookup goes through an
/// index, so its size is all that needs a bound: it is held in memory while it parses.
const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REGISTRY_MIB: u64 = MAX_REGISTRY_BYTES / (1024 * 1024);

/// Why the identity document could not be loaded, with the setting that supplied it:
/// `identity_file` for a file and `identities` for the inline document.
#[derive(Debug, thiserror::Error)]
enum IdentityError {
    /// The document, in either form, exceeds [`MAX_REGISTRY_BYTES`].
    TooLarge(Form),
    /// The identity file could not be read; no other form is read from disk.
    Read(#[source] std::io::Error),
    /// The document, or strict mode's built-in identities, did not load.
    Registry {
        /// `None` when strict mode started without a document, with built-in identities only.
        form: Option<Form>,
        source: RegistryError,
    },
}

impl IdentityError {
    /// The setting that supplied the document. Without one only the built-in identities exist,
    /// so a failure concerns the strict profile itself.
    fn setting(&self) -> Setting {
        let form = match self {
            Self::TooLarge(form) => Some(*form),
            Self::Read(_) => Some(Form::File),
            Self::Registry { form, .. } => *form,
        };
        form.map_or(Setting::AuthMode, |form| {
            DocumentKind::Identities.setting(form)
        })
    }
}

// Startup diagnostics include the setting and the cause's message, and expose the cause.
impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let setting = self.setting().as_str();
        match self {
            Self::TooLarge(_) => write!(f, "{setting} exceeds {MAX_REGISTRY_MIB} MiB"),
            Self::Read(source) => write!(f, "{setting}: {source}"),
            Self::Registry { source, .. } => write!(f, "{setting}: {source}"),
        }
    }
}

/// The identity document's bytes. Inline text is held to the same limit as a file, so both forms
/// accept the same documents.
fn read_identities(source: &DocumentSource) -> Result<Cow<'_, [u8]>, IdentityError> {
    let bytes = match source {
        DocumentSource::File(path) => crate::bounded_file::read(path, MAX_REGISTRY_BYTES)
            .map(Cow::Owned)
            .map_err(|error| match error {
                crate::bounded_file::ReadError::Limit => IdentityError::TooLarge(Form::File),
                crate::bounded_file::ReadError::Io(error) => IdentityError::Read(error),
            })?,
        DocumentSource::Inline(text) => Cow::Borrowed(text.as_bytes()),
        // Only a buckets document is parsed ahead, but its text is the document either way.
        DocumentSource::Parsed(document) => Cow::Borrowed(document.bytes()),
    };
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_REGISTRY_BYTES) {
        return Err(IdentityError::TooLarge(source.form()));
    }
    Ok(bytes)
}

/// Loads `profile`, reading its identity document once; a failure names the setting that
/// supplied the document.
fn load_profile(
    profile: &AuthProfile<DocumentSource>,
    implicit_projects: &BTreeSet<String>,
) -> Result<Auth, IdentityError> {
    let read = profile.try_map_identities(read_identities)?;
    Auth::load(&read, implicit_projects).map_err(|source| IdentityError::Registry {
        form: profile.identities().map(DocumentSource::form),
        source,
    })
}

impl Validation {
    /// Rejects mismatched profiles, so a registry is never silently ignored or missing and a
    /// signature setting never claims what the profile does not do.
    pub(super) fn check_profile_pairing(&mut self, settings: &Settings) {
        match select_profile(&self.draft) {
            Ok(profile) if !self.any_invalid(&PROFILE_SETTINGS) => {
                self.auth_profile = Some(profile);
            }
            Ok(_) => {}
            Err(mismatches) => {
                for mismatch in mismatches {
                    if self.any_invalid(mismatch.depends_on()) {
                        continue;
                    }
                    if let Some(input) = settings.get(&mismatch.reported_at()) {
                        self.diagnostics.errors.push(Diagnostic::new(
                            input.source,
                            &input.key,
                            mismatch.fix(),
                        ));
                    }
                    self.invalid.extend(mismatch.invalidates());
                }
            }
        }
    }
}

impl ConfigurationReport {
    /// The profile to load, when the authentication settings are valid; only tests read it,
    /// because loading takes it from the report itself.
    #[cfg(test)]
    pub(crate) fn auth_profile(&self) -> Option<&AuthProfile<DocumentSource>> {
        self.auth_profile.as_ref()
    }

    /// Loads the profile that validation selected, reading its identity document once, and
    /// checks the principals that settings name against it. Validation owns the pairing rules
    /// and selects how the test signer is chosen, so only the document's contents and those
    /// names can fail here, and only the document settles strict mode's test signer default;
    /// each failure or warning is recorded with the setting it concerns. Without a valid profile
    /// this is permissive, because validation has already reported why startup cannot proceed.
    pub(crate) fn load_auth(&mut self) -> Auth {
        // A copy shares its documents, and it leaves the report free to keep the buckets
        // document that the implicit projects come from.
        let Some(profile) = self.auth_profile.clone() else {
            return Auth::permissive();
        };
        // Only strict mode has built-in identities with implicit roles, and only a built-in
        // identity's implicit roles need the buckets document, so nothing else reads it here.
        let implicit_projects = match &profile {
            AuthProfile::Strict {
                verification,
                trusted,
                ..
            } => match (trusted, verification.test_signer) {
                (TrustedCaller::BuiltIn, _)
                | (_, TestSignerChoice::Fixed(TestSigner::Enabled))
                | (_, TestSignerChoice::WhenDeclared) => {
                    self.parse_buckets_document();
                    self.implicit_projects()
                }
                (
                    TrustedCaller::Anonymous | TrustedCaller::Named(_),
                    TestSignerChoice::Fixed(TestSigner::Disabled),
                ) => BTreeSet::new(),
            },
            AuthProfile::VerifyingPermissive { .. } | AuthProfile::Permissive => BTreeSet::new(),
        };
        // The setting that supplied the identity document, which messages name.
        let registry = profile
            .identities()
            .map(|source| DocumentKind::Identities.setting(source.form()));
        match load_profile(&profile, &implicit_projects) {
            Ok(auth) => {
                self.warn_strict_test_signer(&auth, registry, &implicit_projects);
                self.check_principals(&auth, registry);
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
                BuiltIn::TestSigner.role(),
                projects
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let (setting, cause, remedy) = match (self.draft.test_signer, registry) {
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
        if let Some(input) = self.settings.get(&setting) {
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
    /// `registry` supplied. Loading resolved `trusted_principal` together with the built-in
    /// application identity, and left a name it did not find unresolved.
    fn check_principals(&mut self, auth: &Auth, registry: Option<Setting>) {
        let unknown = match registry {
            Some(registry) => format!(
                "is not an id in {}; use one of its identities",
                registry.as_str()
            ),
            None => {
                "is not a registered identity; declare it in identity_file or identities".to_owned()
            }
        };
        if self.valid(&[Setting::AdminPrincipal])
            && let (Some(principal), Some(principals)) =
                (&self.draft.admin_principal, auth.principals())
            && !principals.iter().any(|identity| identity.id() == principal)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Identity,
                Setting::AdminPrincipal.as_str(),
                format!("admin_principal {principal:?} {unknown}."),
            ));
        }
        // Only strict mode loads a trusted principal; elsewhere validation rejected the setting.
        if self.valid(&[Setting::TrustedPrincipal])
            && let Some(principal) = self.draft.trusted_principal.as_deref()
            && auth.principals().is_some()
            && auth.trusted_principal() != Some(principal)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Identity,
                Setting::TrustedPrincipal.as_str(),
                format!(
                    "trusted_principal {principal:?} {unknown}, the built-in test signer with test_signer=enabled, or omit trusted_principal to act as the built-in {TRUSTED_APP_ID}."
                ),
            ));
        }
    }

    /// Reads and parses the buckets document once and keeps that parse in the configuration.
    /// Authentication loads before the initializer's phase, so the initializer then validates
    /// the same version instead of reading the file again. A document that cannot be read or
    /// parsed fails startup here, with the diagnostic the initializer would give.
    fn parse_buckets_document(&mut self) {
        // Conflicting or invalid document settings were already reported, and nothing loads them.
        if self.valid(&[Setting::BucketsFile, Setting::Buckets])
            && let Some(source) = &self.draft.buckets
        {
            match super::buckets_document::parse_document(source, self.draft.max_buckets_per_batch)
            {
                Ok(document) => self.draft.buckets = Some(DocumentSource::Parsed(document)),
                Err(diagnostics) => self.diagnostics.errors.extend(diagnostics.errors),
            }
        }
    }

    /// Where the built-in identities hold their implicit roles: the effective `seed_project`
    /// and the project of every bucket the parsed buckets document creates, so that the
    /// application can manage the buckets its own setup declares and nothing else.
    fn implicit_projects(&self) -> BTreeSet<String> {
        let mut projects = match &self.draft.buckets {
            Some(DocumentSource::Parsed(document)) => document.projects().clone(),
            Some(DocumentSource::File(_) | DocumentSource::Inline(_)) | None => BTreeSet::new(),
        };
        projects.insert(self.draft.seed_project.clone());
        projects
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = IdentityError::Registry {
        form: None,
        source: RegistryError::Invalid("invalid principal".into()),
    };
    assert_eq!(error.to_string(), "auth_mode: invalid principal");
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<RegistryError>()
    );
    let error = IdentityError::Read(std::io::ErrorKind::PermissionDenied.into());
    assert_eq!(error.to_string(), "identity_file: permission denied");
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<std::io::Error>()
    );
    let error = IdentityError::TooLarge(Form::Inline);
    assert_eq!(error.to_string(), "identities exceeds 16 MiB");
    assert!(std::error::Error::source(&error).is_none());
}
