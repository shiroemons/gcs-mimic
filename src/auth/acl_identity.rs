//! Canonical ACL aliases are trusted registry data, never request claims.

use super::{Identity, MAX_ID_BYTES, RegistryError};
use crate::acl::Entity;
use serde_json::Value;
use std::collections::BTreeMap;

/// Only the domain is DNS-case-insensitive; the local part retains its provider-defined case.
pub(crate) fn same_principal_email(a: &str, b: &str) -> bool {
    match (a.rsplit_once('@'), b.rsplit_once('@')) {
        (Some((local_a, domain_a)), Some((local_b, domain_b))) => {
            local_a == local_b && domain_a.eq_ignore_ascii_case(domain_b)
        }
        (None, None) => a == b,
        _ => false,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct EmailKey(String);

impl EmailKey {
    fn new(raw: &str) -> Self {
        Self(match raw.rsplit_once('@') {
            Some((local, domain)) => format!("{local}@{}", domain.to_ascii_lowercase()),
            None => raw.to_owned(),
        })
    }
}

fn natural_entity(id: &str) -> Option<String> {
    let entity = format!("user-{id}");
    matches!(
        Entity::parse(&entity),
        Ok(Entity::UserEmail(_) | Entity::UserId(_))
    )
    .then_some(entity)
}

/// A creator address must be a canonical ACL user email, so `user-EMAIL` is a valid entity.
#[derive(Debug)]
pub(crate) struct InvalidCreatorEmail;

impl std::fmt::Display for InvalidCreatorEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("must be an email address such as name@example.test")
    }
}

impl std::error::Error for InvalidCreatorEmail {}

pub(crate) fn validate_local_creator_email(email: &str) -> Result<(), InvalidCreatorEmail> {
    let entity = format!("user-{email}");
    if email.len() > 254
        || !matches!(
            crate::acl::Entity::parse(&entity),
            Ok(crate::acl::Entity::UserEmail(_))
        )
    {
        return Err(InvalidCreatorEmail);
    }
    Ok(())
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
            if owners
                .insert(EmailKey::new(&entity), index)
                .is_some_and(|old| old != index)
            {
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
        same_principal_email(&self.id, user)
            || self
                .acl_entity
                .as_deref()
                .and_then(|entity| entity.strip_prefix("user-"))
                .is_some_and(|alias| same_principal_email(alias, user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Registry;
    use serde_json::json;

    #[test]
    fn email_domains_match_without_folding_local_ids_or_local_parts() {
        let registry = Registry::parse(br#"{"identities":[{"id":"alice@Example.COM","tokens":[]},{"id":"local:admin","tokens":[]}]}"#).unwrap();
        let identity = &registry.identities[0];
        assert!(identity.matches_acl_user("alice@example.com"));
        assert!(!identity.matches_acl_user("Alice@example.com"));
        assert!(!registry.identities[1].matches_acl_user("LOCAL:admin"));
        for (member, allowed) in [
            ("user:alice@EXAMPLE.com", true),
            ("user:Alice@example.com", false),
        ] {
            let policy = crate::auth::Policy {
                bindings: vec![crate::auth::Binding {
                    role: "roles/storage.objectViewer".into(),
                    members: vec![member.into()],
                }],
                etag: None,
            };
            assert_eq!(
                policy.allows(
                    Some(identity),
                    crate::auth::OBJECTS_GET,
                    crate::auth::PublicAccess::Allowed
                ),
                allowed
            );
        }
        assert!(Registry::parse(br#"{"identities":[{"id":"a@Example.com","tokens":[]},{"id":"a@example.com","tokens":[]}]}"#).is_err());
        assert!(Registry::parse(br#"{"identities":[{"id":"A@example.com","tokens":[]},{"id":"a@example.com","tokens":[]}]}"#).is_ok());
        let entries = crate::acl::parse_entries(
            crate::acl::AclKind::Object,
            vec![
                crate::acl::AclEntryInput {
                    entity: "user-a@Example.com".into(),
                    role: "READER".into(),
                },
                crate::acl::AclEntryInput {
                    entity: "user-a@example.com".into(),
                    role: "READER".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entity, "user-a@Example.com");
    }

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
