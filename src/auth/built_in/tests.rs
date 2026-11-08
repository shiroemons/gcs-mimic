use super::*;
use crate::auth::{
    BUCKETS_CREATE, BUCKETS_SET_IAM, Caller, ListenerRole, OBJECTS_CREATE, OBJECTS_GET, roles,
};
use serde_json::json;

fn registry(identities: serde_json::Value) -> Registry {
    Registry::parse(json!({ "identities": identities }).to_string().as_bytes()).unwrap()
}

fn projects(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn admin() -> serde_json::Value {
    json!({"id":"admin@example.invalid","tokens":["admin"]})
}

#[test]
fn every_implicit_role_is_in_the_pinned_snapshot() {
    for built_in in [BuiltIn::TestSigner, BuiltIn::TrustedApp] {
        let role = built_in.role();
        assert!(roles::permissions(role).is_some(), "{role}");
        assert_eq!(built_in.grants(&projects(&["p"])).len(), 1, "{role}");
    }
}

/// The application identity manages the projects it was given and no others, and only the
/// trusted listener can act as it: it has no token and no key. Its role is implicit, so it never
/// counts as the explicit grant that sync controls require.
#[test]
fn the_built_in_app_holds_storage_admin_in_its_projects_only_on_the_trusted_listener() {
    let mut registry = registry(json!([admin()]));
    registry
        .trust(
            &TrustedCaller::BuiltIn,
            &projects(&["local-project", "project-a"]),
        )
        .unwrap();
    assert_eq!((registry.declared, registry.len()), (1, 2));
    assert!(!registry.declares(TRUSTED_APP_ID));
    let app = registry.identity(TRUSTED_APP_ID).unwrap();
    for project in ["local-project", "project-a"] {
        for permission in [BUCKETS_CREATE, BUCKETS_SET_IAM, OBJECTS_CREATE, OBJECTS_GET] {
            assert!(app.allows(permission, project), "{project}");
        }
        assert!(!app.has_explicit_storage_admin(project), "{project}");
    }
    assert!(!app.allows(BUCKETS_CREATE, "other-project"));
    assert_eq!(
        app.acl_entity().as_deref(),
        Some("user-trusted-app@gcs-mimic.invalid")
    );
    assert!(registry.tokens.values().all(|index| *index != 1));
    assert!(!registry.signing_keys.contains(TRUSTED_APP_ID));
    let Caller::Principal(trusted) = registry.caller_without_credentials(ListenerRole::Trusted)
    else {
        panic!("the trusted listener acts as the built-in app");
    };
    assert_eq!(trusted.id(), TRUSTED_APP_ID);
    assert!(matches!(
        registry.caller_without_credentials(ListenerRole::Public),
        Caller::Anonymous
    ));
}

/// An explicit entry replaces the implicit grants: its roles, tokens and alias stay its own.
#[test]
fn a_declared_app_keeps_its_own_entry_instead_of_the_implicit_grants() {
    let mut registry = registry(json!([admin(), {
        "id": TRUSTED_APP_ID,
        "tokens": ["app-token"],
        "acl_entity": "user-app-alias@example.invalid",
        "project_roles": {"local-project": ["roles/storage.objectViewer"]}
    }]));
    registry
        .trust(
            &TrustedCaller::BuiltIn,
            &projects(&["local-project", "project-a"]),
        )
        .unwrap();
    assert_eq!((registry.declared, registry.len()), (2, 2));
    assert!(registry.declares(TRUSTED_APP_ID));
    let app = registry.identity(TRUSTED_APP_ID).unwrap();
    assert!(app.allows(OBJECTS_GET, "local-project"));
    assert!(!app.allows(OBJECTS_CREATE, "local-project"));
    assert!(!app.allows(OBJECTS_GET, "project-a"));
    assert_eq!(
        app.acl_entity().as_deref(),
        Some("user-app-alias@example.invalid")
    );
    assert_eq!(registry.tokens["app-token"], 1);
    assert_eq!(registry.trusted, Some(1));
}

/// Only a role written in the identity document is explicit: the implicit roles of both built-in
/// identities grant their permissions but never the authority of a declared storage.admin.
#[test]
fn only_a_declared_storage_admin_is_explicit() {
    let mut registry = registry(json!([{
        "id": TRUSTED_APP_ID,
        "tokens": [],
        "project_roles": {"declared-project": ["roles/storage.admin"]}
    }]));
    registry
        .trust(&TrustedCaller::BuiltIn, &projects(&["local-project"]))
        .unwrap();
    let app = registry.identity(TRUSTED_APP_ID).unwrap();
    assert!(app.has_explicit_storage_admin("declared-project"));
    assert!(!app.has_explicit_storage_admin("local-project"));
    for built_in in [BuiltIn::TestSigner, BuiltIn::TrustedApp] {
        for grant in built_in.grants(&projects(&["p"])).values() {
            assert!(!grant.explicit_storage_admin, "{built_in:?}");
            assert!(grant.any_storage_permission, "{built_in:?}");
        }
    }
}

#[test]
fn an_alias_naming_the_app_fails_instead_of_sharing_its_acl_entity() {
    for identity in [
        json!({"id":"other@example.invalid","tokens":[],"acl_entity":"user-trusted-app@GCS-MIMIC.invalid"}),
        json!({"id":"trusted-app@GCS-MIMIC.INVALID","tokens":[]}),
    ] {
        let mut registry = registry(json!([identity]));
        let error = registry
            .trust(&TrustedCaller::BuiltIn, &projects(&["local-project"]))
            .unwrap_err();
        assert!(matches!(error, RegistryError::Invalid { entry: None, .. }));
        assert!(error.to_string().contains("trusted_principal"), "{error}");
    }
}
