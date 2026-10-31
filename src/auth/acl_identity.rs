//! Canonical ACL aliases are trusted registry data, never request claims.

use super::{Identity, MAX_ID_BYTES, RegistryError};
use crate::acl::Entity;
use serde_json::Value;
use std::collections::BTreeMap;

fn natural_entity(id: &str) -> Option<String> {
    let entity = format!("user-{id}");
    matches!(
        Entity::parse(&entity),
        Ok(Entity::UserEmail(_) | Entity::UserId(_))
    )
    .then_some(entity)
}

pub(super) fn parse_entity(
    value: Option<&Value>,
    index: usize,
) -> Result<Option<String>, RegistryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let invalid = || RegistryError::Invalid {
        entry: Some(index),
        reason: "acl_entity must be a canonical user-EMAIL or user-ID of at most 259 bytes".into(),
    };
    let entity = value
        .as_str()
        .filter(|entity| entity.len() <= MAX_ID_BYTES + 5)
        .ok_or_else(invalid)?;
    if !matches!(
        Entity::parse(entity),
        Ok(Entity::UserEmail(_) | Entity::UserId(_))
    ) {
        return Err(invalid());
    }
    Ok(Some(entity.to_owned()))
}

pub(super) fn validate_aliases(identities: &[Identity]) -> Result<(), RegistryError> {
    let mut owners = BTreeMap::new();
    for (index, identity) in identities.iter().enumerate() {
        for entity in [natural_entity(&identity.id), identity.acl_entity()]
            .into_iter()
            .flatten()
        {
            if owners.insert(entity, index).is_some_and(|old| old != index) {
                return Err(RegistryError::Invalid {
                    entry: Some(index),
                    reason:
                        "acl_entity collides with another principal's alias or natural user entity"
                            .into(),
                });
            }
        }
    }
    Ok(())
}

impl Identity {
    pub(crate) fn acl_entity(&self) -> Option<String> {
        self.acl_entity.clone().or_else(|| natural_entity(&self.id))
    }

    pub(crate) fn matches_acl_user(&self, user: &str) -> bool {
        self.id == user
            || self
                .acl_entity
                .as_deref()
                .and_then(|entity| entity.strip_prefix("user-"))
                == Some(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Registry;
    use serde_json::json;

    #[test]
    fn aliases_preserve_local_ids_without_fabricating_canonical_entities() {
        let registry = Registry::parse(
            br#"{"identities":[{"id":"local:admin-user","tokens":[],"acl_entity":"user-admin@example.test"},{"id":"local:unknown","tokens":[]},{"id":"natural@example.test","tokens":[]}]}"#,
        )
        .unwrap();
        let aliased = &registry.identities[0];
        assert_eq!(aliased.id(), "local:admin-user");
        assert_eq!(
            aliased.acl_entity().as_deref(),
            Some("user-admin@example.test")
        );
        assert!(aliased.matches_acl_user("admin@example.test"));
        assert!(!aliased.matches_acl_user("natural@example.test"));
        for member in ["user:admin@example.test", "serviceAccount:local:admin-user"] {
            let policy = crate::auth::Policy {
                bindings: vec![crate::auth::Binding {
                    role: "roles/storage.objectViewer".into(),
                    members: vec![member.into()],
                }],
                etag: None,
            };
            assert!(policy.allows(
                Some(aliased),
                crate::auth::OBJECTS_GET,
                crate::auth::PublicAccess::Allowed
            ));
            assert!(!policy.allows(
                Some(&registry.identities[2]),
                crate::auth::OBJECTS_GET,
                crate::auth::PublicAccess::Allowed
            ));
        }
        assert_eq!(registry.identities[1].acl_entity(), None);
        assert_eq!(
            registry.identities[2].acl_entity().as_deref(),
            Some("user-natural@example.test")
        );
        let mut snapshot = aliased.upload_snapshot();
        snapshot["acl_entity"] = json!("user-natural@example.test");
        let restored = Identity::from_upload_snapshot(&snapshot).unwrap();
        assert!(!restored.matches_acl_user("natural@example.test"));
        assert_eq!(restored.id(), aliased.id());
    }

    #[test]
    fn aliases_reject_untrusted_scopes_and_cross_principal_collisions() {
        for entity in [
            json!(null),
            json!(1),
            json!("user-bad-id"),
            json!("group-a@example.test"),
            json!("domain-example.test"),
            json!("project-owners-1"),
            json!(format!("user-{}", "a".repeat(255))),
        ] {
            assert!(
                Registry::parse(
                    json!({"identities":[{"id":"local:admin","tokens":[],"acl_entity":entity}]})
                        .to_string()
                        .as_bytes()
                )
                .is_err()
            );
        }
        for identities in [
            json!([{"id":"local:first","tokens":[],"acl_entity":"user-shared"},{"id":"local:second","tokens":[],"acl_entity":"user-shared"}]),
            json!([{"id":"natural","tokens":[]},{"id":"local:second","tokens":[],"acl_entity":"user-natural"}]),
            json!([{"id":"local:first","tokens":[],"acl_entity":"user-natural"},{"id":"natural","tokens":[]}]),
        ] {
            assert!(
                Registry::parse(json!({"identities":identities}).to_string().as_bytes()).is_err()
            );
        }
    }
}
