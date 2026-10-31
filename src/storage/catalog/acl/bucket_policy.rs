//! Legacy bucket IAM roles and bucket ACLs are two views of the same authority.

use super::{
    AclEntry, CatalogError, Entity, UPSERT_BUCKET_ENTRIES, bucket_row, stored_bucket_entries,
};
use crate::{
    acl::{AclKind, bucket_iam_role, project_number, project_private, validate_entries},
    auth::{Binding, Policy},
    storage::catalog::{policies, principals::Profile, unavailable},
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

fn acl_role(role: &str) -> Option<&'static str> {
    match role {
        "roles/storage.legacyBucketReader" => Some("READER"),
        "roles/storage.legacyBucketWriter" => Some("WRITER"),
        "roles/storage.legacyBucketOwner" => Some("OWNER"),
        _ => None,
    }
}

fn member_entity(member: &str, project: &str, principals: Option<&Profile>) -> Option<String> {
    if matches!(member, "allUsers" | "allAuthenticatedUsers") {
        return Some(member.into());
    }
    let (kind, id) = member.split_once(':')?;
    if matches!(kind, "user" | "serviceAccount") {
        if let Some(principals) = principals {
            return principals.canonical_acl_entity(id);
        }
        let entity = format!("user-{id}");
        return Entity::parse(&entity).is_ok().then_some(entity);
    }
    let entity = match kind {
        "group" => format!("group-{id}"),
        "domain" => format!("domain-{id}"),
        "projectOwner" | "projectEditor" | "projectViewer" => {
            let team = match kind {
                "projectOwner" => "owners",
                "projectEditor" => "editors",
                _ => "viewers",
            };
            let number = if id == project {
                project_number(project)
            } else {
                id.into()
            };
            format!("project-{team}-{number}")
        }
        _ => return None,
    };
    Entity::parse(&entity).is_ok().then_some(entity)
}

fn iam_member(entity: &str, project: &str) -> Result<String, CatalogError> {
    Ok(match Entity::parse(entity).map_err(CatalogError::from)? {
        Entity::AllUsers | Entity::AllAuthenticatedUsers => entity.into(),
        Entity::UserEmail(id) | Entity::UserId(id) => format!("user:{id}"),
        Entity::GroupEmail(id) | Entity::GroupId(id) => format!("group:{id}"),
        Entity::Domain(domain) => format!("domain:{domain}"),
        Entity::ProjectTeam { team, project: id } => {
            let kind = match team {
                "owners" => "projectOwner",
                "editors" => "projectEditor",
                _ => "projectViewer",
            };
            let id = if id == project_number(project) {
                project
            } else {
                id
            };
            format!("{kind}:{id}")
        }
    })
}

fn rank(role: &str) -> u8 {
    match role {
        "OWNER" => 3,
        "WRITER" => 2,
        _ => 1,
    }
}

fn policy_entries(policy: &Policy, project: &str, principals: Option<&Profile>) -> Vec<AclEntry> {
    let mut entries: BTreeMap<String, &str> = BTreeMap::new();
    for binding in &policy.bindings {
        let Some(role) = acl_role(&binding.role) else {
            continue;
        };
        for member in &binding.members {
            if let Some(entity) = member_entity(member, project, principals) {
                let current = entries.entry(entity).or_insert(role);
                if rank(role) > rank(current) {
                    *current = role;
                }
            }
        }
    }
    entries
        .into_iter()
        .map(|(entity, role)| AclEntry {
            entity,
            role: role.into(),
        })
        .collect()
}

fn mark_bound(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    connection
        .prepare_cached(
            "UPDATE buckets SET acl_policy_bound=1 WHERE name=?1 AND acl_policy_bound=0",
        )
        .and_then(|mut statement| statement.execute([bucket]))
        .map_err(unavailable)?;
    Ok(())
}

fn merge_stored_grants(
    policy: &mut Policy,
    entries: &[AclEntry],
    project: &str,
    principals: Option<&Profile>,
) -> Result<bool, CatalogError> {
    validate_entries(AclKind::Bucket, entries).map_err(CatalogError::from)?;
    let mut changed = false;
    for entry in entries {
        let role = bucket_iam_role(&entry.role)
            .ok_or_else(|| CatalogError::Corrupt("invalid bucket ACL role".into()))?;
        let present = policy.bindings.iter().any(|binding| {
            binding.role == role
                && binding.members.iter().any(|member| {
                    member_entity(member, project, principals).as_deref()
                        == Some(entry.entity.as_str())
                })
        });
        if !present {
            policy.bindings.push(Binding {
                role: role.into(),
                members: vec![iam_member(&entry.entity, project)?],
            });
            changed = true;
        }
    }
    Ok(changed)
}

pub(super) fn replace_policy(
    connection: &Connection,
    bucket: &str,
    entries: &[AclEntry],
    principals: Option<&Profile>,
) -> Result<(), CatalogError> {
    let row = bucket_row(connection, bucket)?.ok_or(CatalogError::NotFound)?;
    let mut policy = policies::read_policy(connection, bucket)?;
    let previous = stored_bucket_entries(connection, bucket, "bucket")?
        .unwrap_or_else(|| project_private(&row.project));
    let initialized = if row.policy_bound {
        false
    } else {
        merge_stored_grants(&mut policy, &previous, &row.project, principals)?
    };
    let before: BTreeMap<_, _> = previous
        .iter()
        .map(|entry| (&entry.entity, &entry.role))
        .collect();
    let after: BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (&entry.entity, &entry.role))
        .collect();
    let changed: std::collections::BTreeSet<_> = before
        .keys()
        .chain(after.keys())
        .filter(|entity| before.get(**entity) != after.get(**entity))
        .map(|entity| (*entity).clone())
        .collect();
    if changed.is_empty() {
        if initialized {
            policies::store_policy(connection, bucket, &mut policy)?;
        }
        return mark_bound(connection, bucket);
    }
    let mut members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for binding in &mut policy.bindings {
        if acl_role(&binding.role).is_some() {
            binding.members.retain(|member| {
                let Some(entity) = member_entity(member, &row.project, principals) else {
                    return true;
                };
                if !changed.contains(&entity) {
                    return true;
                }
                members.entry(entity).or_default().push(member.clone());
                false
            });
        }
    }
    policy
        .bindings
        .retain(|binding| !binding.members.is_empty());
    let mut bindings: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for entry in entries {
        if !changed.contains(&entry.entity) {
            continue;
        }
        let role = bucket_iam_role(&entry.role)
            .ok_or_else(|| CatalogError::Corrupt("invalid bucket ACL role".into()))?;
        // An ACL edit must not turn a stable IAM local ID into its changeable user alias.
        let grantees = members.remove(&entry.entity).unwrap_or_default();
        if grantees.is_empty() {
            bindings
                .entry(role)
                .or_default()
                .push(iam_member(&entry.entity, &row.project)?);
        } else {
            bindings.entry(role).or_default().extend(grantees);
        }
    }
    policy
        .bindings
        .extend(bindings.into_iter().map(|(role, mut members)| {
            members.sort();
            members.dedup();
            Binding {
                role: role.into(),
                members,
            }
        }));
    policies::store_policy(connection, bucket, &mut policy)?;
    mark_bound(connection, bucket)
}

pub(in super::super) fn replace_from_policy(
    connection: &Connection,
    bucket: &str,
    policy: &Policy,
    principals: Option<&Profile>,
) -> Result<bool, CatalogError> {
    let row = bucket_row(connection, bucket)?.ok_or(CatalogError::NotFound)?;
    let entries = policy_entries(policy, &row.project, principals);
    validate_entries(AclKind::Bucket, &entries).map_err(CatalogError::from)?;
    let mut current = stored_bucket_entries(connection, bucket, "bucket")?
        .unwrap_or_else(|| project_private(&row.project));
    current.sort_by(|left, right| left.entity.cmp(&right.entity));
    if current == entries {
        mark_bound(connection, bucket)?;
        return Ok(false);
    }
    let encoded = serde_json::to_string(&entries).map_err(unavailable)?;
    connection
        .prepare_cached(UPSERT_BUCKET_ENTRIES)
        .and_then(|mut statement| statement.execute(params![bucket, "bucket", encoded]))
        .map_err(unavailable)?;
    mark_bound(connection, bucket)?;
    Ok(true)
}

pub(in super::super) fn reconcile(
    connection: &Connection,
    bucket: &str,
    principals: &Profile,
    updated: i64,
) -> Result<(), CatalogError> {
    let row = bucket_row(connection, bucket)?.ok_or(CatalogError::NotFound)?;
    let mut policy = policies::read_policy(connection, bucket)?;
    let changed = if row.policy_bound {
        false
    } else if let Some(entries) = stored_bucket_entries(connection, bucket, "bucket")? {
        merge_stored_grants(&mut policy, &entries, &row.project, Some(principals))?
    } else {
        false
    };
    let acl_changed = replace_from_policy(connection, bucket, &policy, Some(principals))?;
    if changed {
        policies::store_policy(connection, bucket, &mut policy)?;
        crate::storage::catalog::change_journal::append(
            connection,
            crate::storage::admin::Origin::Api,
            crate::storage::catalog::change_journal::ChangeKind::Iam,
            bucket,
            None,
        )?;
    }
    if acl_changed {
        policies::advance_acl_revision(connection, bucket, updated)?;
        crate::storage::catalog::change_journal::append(
            connection,
            crate::storage::admin::Origin::Api,
            crate::storage::catalog::change_journal::ChangeKind::BucketUpdate,
            bucket,
            None,
        )?;
    }
    Ok(())
}
