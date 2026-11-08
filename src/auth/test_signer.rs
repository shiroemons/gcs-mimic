//! The built-in test signer: an identity whose RSA key pair is published with the repository.
//!
//! Anyone can sign as this identity, so its signatures prove nothing about who sent a request.
//! It lets local tests and apps sign URLs without generating and registering a key;
//! `test_signer=disabled` removes it.

use super::signature::PublicKey;

/// The signer ID. The reserved `.invalid` domain can never name a real Google account, and the
/// ID differs from the permissive default creator `local-user@gcs-mimic.invalid`.
pub const ID: &str = "test-signer@gcs-mimic.invalid";

/// The published private key. It is public, so it must never sign anything but local tests.
pub const PRIVATE_KEY_PEM: &str = include_str!("../../fixtures/signing/test-only-private.pem");

const PUBLIC_KEY_PEM: &str = include_str!("../../fixtures/signing/test-only-public.pem");

/// The project the credentials name, the emulator's default seed and guide project.
const PROJECT_ID: &str = "local-project";

/// A universe other than `googleapis.com` makes Google auth libraries sign their own tokens
/// instead of asking Google OAuth for one; the Rails guide uses the same value.
const UNIVERSE_DOMAIN: &str = "gcs-emulator.invalid";

#[expect(
    clippy::expect_used,
    reason = "the embedded key is a compile-time constant that a unit test parses"
)]
pub(super) fn public_key() -> PublicKey {
    PublicKey::parse(PUBLIC_KEY_PEM).expect("the embedded test signer key is an RSA SPKI PEM")
}

/// The signer as a service account key file. Images and binaries hold no copy of the repository,
/// so this is how their users get the published key.
pub fn service_account_json() -> String {
    let credentials = serde_json::json!({
        "type": "service_account",
        "project_id": PROJECT_ID,
        "client_email": ID,
        "private_key": PRIVATE_KEY_PEM,
        "universe_domain": UNIVERSE_DOMAIN,
    });
    format!("{credentials:#}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{OBJECTS_GET, Registry, RegistryError};
    use serde_json::json;

    fn registry(identities: serde_json::Value) -> Registry {
        Registry::parse(json!({ "identities": identities }).to_string().as_bytes()).unwrap()
    }

    #[test]
    fn an_undeclared_signer_exists_without_project_roles_or_tokens() {
        let mut registry = registry(json!([{"id":"admin@example.invalid","tokens":["admin"]}]));
        registry.add_test_signer().unwrap();
        assert_eq!(registry.declared, 1);
        assert_eq!(registry.len(), 2);
        let signer = registry.identity(ID).unwrap();
        assert!(!signer.allows_anywhere(OBJECTS_GET));
        assert_eq!(
            signer.acl_entity().as_deref(),
            Some("user-test-signer@gcs-mimic.invalid")
        );
        assert_eq!(registry.signing_keys.get(ID).unwrap().len(), 1);
        assert_eq!(registry.tokens.len(), 1);
    }

    #[test]
    fn a_declared_signer_keeps_its_grants_and_tokens_and_gains_the_built_in_key() {
        let mut registry = registry(json!([{
            "id": ID,
            "tokens": ["signer-token"],
            "public_keys": [PUBLIC_KEY_PEM],
            "acl_entity": "user-signer-alias@example.invalid",
            "project_roles": {"local": ["roles/storage.objectViewer"]}
        }]));
        registry.add_test_signer().unwrap();
        assert_eq!((registry.declared, registry.len()), (1, 1));
        let signer = registry.identity(ID).unwrap();
        assert!(signer.allows(OBJECTS_GET, "local"));
        assert_eq!(
            signer.acl_entity().as_deref(),
            Some("user-signer-alias@example.invalid")
        );
        assert_eq!(registry.signing_keys.get(ID).unwrap().len(), 2);
        assert_eq!(registry.tokens["signer-token"], 0);
    }

    #[test]
    fn an_alias_naming_the_signer_fails_instead_of_sharing_its_acl_entity() {
        for identity in [
            json!({"id":"other@example.invalid","tokens":[],"acl_entity":"user-test-signer@GCS-MIMIC.invalid"}),
            json!({"id":"test-signer@GCS-MIMIC.INVALID","tokens":[]}),
        ] {
            let mut registry = registry(json!([identity]));
            let error = registry.add_test_signer().unwrap_err();
            assert!(matches!(error, RegistryError::Invalid { entry: None, .. }));
            assert!(
                error.to_string().contains("test_signer=disabled"),
                "{error}"
            );
        }
    }

    #[test]
    fn the_embedded_key_pair_signs_and_verifies() {
        let message = "test signer message";
        let signature =
            crate::auth::signature::sign_policy(message, PRIVATE_KEY_PEM.as_bytes()).unwrap();
        let bytes = crate::hex::decode::<256>(&signature).unwrap();
        let key = public_key();
        assert!(key.verify(message.as_bytes(), &bytes));
        assert!(!key.verify(b"another message", &bytes));
    }

    #[test]
    fn the_service_account_key_signs_what_the_embedded_public_key_verifies() {
        let credentials: serde_json::Value = serde_json::from_str(&service_account_json()).unwrap();
        assert_eq!(credentials["type"], "service_account");
        assert_eq!(credentials["client_email"], ID);
        assert_eq!(credentials["project_id"], "local-project");
        assert_eq!(credentials["universe_domain"], "gcs-emulator.invalid");
        let private_key = credentials["private_key"].as_str().unwrap();
        let message = "signed with the printed key";
        let signature =
            crate::auth::signature::sign_policy(message, private_key.as_bytes()).unwrap();
        let bytes = crate::hex::decode::<256>(&signature).unwrap();
        assert!(public_key().verify(message.as_bytes(), &bytes));
    }
}
