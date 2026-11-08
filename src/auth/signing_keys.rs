//! RSA public keys by signer ID, shared by strict principals and permissive verification.

use super::signature::PublicKey;
use std::{collections::HashMap, sync::Arc};

/// Requests resolve a signer per request, so lookups never walk the identities. Keys are shared
/// slices because every verification job takes its own reference.
#[derive(Debug, Default)]
pub(super) struct SigningKeys(HashMap<String, Arc<[PublicKey]>>);

impl SigningKeys {
    pub(super) fn get(&self, signer: &str) -> Option<&Arc<[PublicKey]>> {
        self.0.get(signer)
    }

    pub(super) fn contains(&self, signer: &str) -> bool {
        self.0.contains_key(signer)
    }

    /// Records a declared identity's keys; the registry rejects duplicate IDs first.
    pub(super) fn insert(&mut self, signer: &str, keys: Vec<PublicKey>) {
        self.0.insert(signer.to_owned(), keys.into());
    }

    /// Appends `key` to the signer's keys, keeping any it already has.
    pub(super) fn add(&mut self, signer: &str, key: PublicKey) {
        let keys = self.0.remove(signer);
        self.0.insert(
            signer.to_owned(),
            keys.iter()
                .flat_map(|keys| keys.iter().cloned())
                .chain([key])
                .collect(),
        );
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::auth::{
        Auth, AuthMode, IdentityUse, Profile, Registry, SignedUrls, TestSigner,
        signature::SignedHostPort, test_signer,
    };
    use crate::config::{AuthProfile, DocumentSource, Verification};
    use serde_json::json;

    const PUBLIC: &str = include_str!("../../fixtures/signing/test-only-public.pem");

    fn parse(identities: serde_json::Value) -> Result<Registry, String> {
        Registry::parse_for(identities.to_string().as_bytes(), IdentityUse::SigningKeys)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn a_permissive_identity_file_supplies_only_ids_and_public_keys() {
        for entry in [
            json!({"id":"signer@example.invalid","public_keys":[PUBLIC]}),
            json!({"id":"signer@example.invalid","tokens":[],"public_keys":[PUBLIC]}),
        ] {
            let registry = parse(json!({"identities":[entry]})).unwrap();
            assert!(registry.signing_keys.contains("signer@example.invalid"));
        }
        for (entry, field) in [
            (
                json!({"project_roles":{"local":["roles/storage.admin"]}}),
                "project_roles",
            ),
            (
                json!({"acl_entity":"user-alias@example.invalid"}),
                "acl_entity",
            ),
            (json!({"tokens":["token"]}), "tokens"),
        ] {
            let mut entry = entry;
            entry["id"] = json!("signer@example.invalid");
            entry["public_keys"] = json!([PUBLIC]);
            let error = parse(json!({"identities":[entry]})).unwrap_err();
            assert!(
                error.contains(&format!("identities[0]: {field} requires auth_mode=strict")),
                "{error}"
            );
        }
        let error = parse(
            json!({"identities":[{"id":"signer@example.invalid","public_keys":[PUBLIC]}],
            "public_access_prevention_projects":["local"]}),
        )
        .unwrap_err();
        assert!(error.contains("public_access_prevention_projects requires auth_mode=strict"));
        let error = parse(json!({"identities":[{"id":"signer@example.invalid"}]})).unwrap_err();
        assert!(
            error.contains("missing required field: public_keys"),
            "{error}"
        );
    }

    #[test]
    fn load_selects_the_profile_and_its_signers() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.0.join("identities.json");
        std::fs::write(
            &path,
            json!({"identities":[{"id":"file@example.invalid","public_keys":[PUBLIC]}]})
                .to_string(),
        )
        .unwrap();
        let auth = Auth::load(&AuthProfile::Permissive).unwrap();
        assert!(matches!(auth.profile, Profile::Permissive));
        assert_eq!(auth.signed_urls(), SignedUrls::Ignore);
        for (test_signer, identity_file, signed_host_port) in [
            (TestSigner::Enabled, false, SignedHostPort::Exact),
            (TestSigner::Disabled, false, SignedHostPort::Lenient),
            (TestSigner::Enabled, true, SignedHostPort::Lenient),
            (TestSigner::Disabled, true, SignedHostPort::Exact),
        ] {
            let auth = Auth::load(&AuthProfile::VerifyingPermissive {
                identities: identity_file.then(|| DocumentSource::File(path.clone())),
                verification: Verification {
                    test_signer,
                    signed_host_port,
                },
            })
            .unwrap();
            assert_eq!(auth.mode(), AuthMode::Permissive);
            assert_eq!(auth.signed_urls(), SignedUrls::Verify);
            assert_eq!(auth.signed_host_port, signed_host_port);
            assert!(auth.principals().is_none() && auth.identity_count().is_none());
            let Profile::VerifyingPermissive(keys) = &auth.profile else {
                panic!("permissive verification expected");
            };
            assert_eq!(keys.contains("file@example.invalid"), identity_file);
            assert_eq!(
                keys.contains(test_signer::ID),
                test_signer == TestSigner::Enabled
            );
        }
        // Loading reports only identity file contents; the pairing rules belong to validation.
        let error = Auth::load(&AuthProfile::Strict {
            identities: DocumentSource::File(path),
            verification: Verification {
                test_signer: TestSigner::Disabled,
                signed_host_port: SignedHostPort::Lenient,
            },
        })
        .unwrap_err();
        assert!(error.to_string().contains("missing required field: tokens"));
    }
}
