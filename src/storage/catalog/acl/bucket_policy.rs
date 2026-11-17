//! Legacy bucket IAM roles and bucket ACLs are two views of the same authority.

use super::{
    AclEntry, BucketScope, CatalogError, UPSERT_BUCKET_ENTRIES, bucket_row, stored_bucket_entries,
};
use crate::storage::catalog::sql;
use crate::{
    acl::{AclKind, AclRole, Entity, EntityRef, parse_entity, project_private, validate_entries},
    auth::{Binding, Member, MemberRef, Policy, Role},
    project::ProjectNumber,
    storage::catalog::{policies, principals::Profile, unavailable},
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// The ACL entity a member names, if it forms one within the size cap.
fn member_entity(member: &Member, project: &str, principals: Option<&Profile>) -> Option<Entity> {
    let entity = match member.parsed() {
        MemberRef::AllUsers => return Some(Entity::all_users()),
        MemberRef::AllAuthenticatedUsers => return Some(Entity::all_authenticated_users()),
        MemberRef::User(id) | MemberRef::ServiceAccount(id) => {
            return match principals {
                Some(principals) => principals.canonical_acl_entity(id),
                None => Entity::from_user_id(id),
            };
        }
        MemberRef::Group(id) => format!("group-{id}"),
        MemberRef::Domain(id) => format!("domain-{id}"),
        MemberRef::Project { team, project: id } if id == project => {
            return Some(Entity::project_team(team, project));
        }
        MemberRef::Project { team, project: id } => format!("project-{}-{id}", team.as_str()),
    };
    parse_entity(&entity).ok()
}

fn iam_member(entity: &Entity, project: &str) -> Member {
    Member::from(match entity.parsed() {
        EntityRef::AllUsers => MemberRef::AllUsers,
        EntityRef::AllAuthenticatedUsers => MemberRef::AllAuthenticatedUsers,
        EntityRef::UserEmail(id) | EntityRef::UserId(id) => MemberRef::User(id),
        EntityRef::GroupEmail(id) | EntityRef::GroupId(id) => MemberRef::Group(id),
        EntityRef::Domain(domain) => MemberRef::Domain(domain),
        EntityRef::ProjectTeam { team, project: id } => MemberRef::Project {
            team,
            project: if id == ProjectNumber::of(project).to_string() {
                project
            } else {
                id
            },
        },
    })
}

fn policy_entries(policy: &Policy, project: &str, principals: Option<&Profile>) -> Vec<AclEntry> {
    let mut entries: BTreeMap<Entity, AclRole> = BTreeMap::new();
    for binding in &policy.bindings {
        let Some(role) = AclRole::from_bucket_iam_role(binding.role) else {
            continue;
        };
        for member in &binding.members {
            if let Some(entity) = member_entity(member, project, principals) {
                let current = entries.entry(entity).or_insert(role);
                if role > *current {
                    *current = role;
                }
            }
        }
    }
    entries
        .into_iter()
        .map(|(entity, role)| AclEntry { entity, role })
        .collect()
}

fn mark_bound(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    sql::exec(
        connection,
        "UPDATE buckets SET acl_policy_bound=1 WHERE name=?1 AND acl_policy_bound=0",
        [bucket],
    )?;
    Ok(())
}

fn merge_stored_grants(
    policy: &mut Policy,
    entries: &[AclEntry],
    project: &str,
    principals: Option<&Profile>,
) -> Result<bool, CatalogError> {
    validate_entries(AclKind::Bucket, entries)?;
    let mut changed = false;
    for entry in entries {
        let role = entry.role.bucket_iam_role();
        let present = policy.bindings.iter().any(|binding| {
            binding.role == role
                && binding.members.iter().any(|member| {
                    member_entity(member, project, principals).as_ref() == Some(&entry.entity)
                })
        });
        if !present {
            policy.bindings.push(Binding {
                role,
                members: vec![iam_member(&entry.entity, project)],
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
    let previous = stored_bucket_entries(connection, bucket, BucketScope::Bucket)?
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
    let mut members: BTreeMap<Entity, Vec<Member>> = BTreeMap::new();
    for binding in &mut policy.bindings {
        if AclRole::from_bucket_iam_role(binding.role).is_some() {
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
    // Role order is name order, so bindings are appended in the order the names sort.
    let mut bindings: BTreeMap<Role, Vec<Member>> = BTreeMap::new();
    for entry in entries {
        if !changed.contains(&entry.entity) {
            continue;
        }
        let role = entry.role.bucket_iam_role();
        // An ACL edit must not turn a stable IAM local ID into its changeable user alias.
        let grantees = members.remove(&entry.entity).unwrap_or_default();
        if grantees.is_empty() {
            bindings
                .entry(role)
                .or_default()
                .push(iam_member(&entry.entity, &row.project));
        } else {
            bindings.entry(role).or_default().extend(grantees);
        }
    }
    for (role, mut members) in bindings {
        members.sort();
        members.dedup();
        // A member whose canonical entity is not the one an edit named stayed bound above, because
        // that entity did not change. Adding it again would grow the policy with every edit.
        members.retain(|member| {
            !policy
                .bindings
                .iter()
                .any(|binding| binding.role == role && binding.members.contains(member))
        });
        if !members.is_empty() {
            policy.bindings.push(Binding { role, members });
        }
    }
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
    validate_entries(AclKind::Bucket, &entries)?;
    let mut current = stored_bucket_entries(connection, bucket, BucketScope::Bucket)?
        .unwrap_or_else(|| project_private(&row.project));
    let stored_order = current.clone();
    current.sort_by(|left, right| left.entity.cmp(&right.entity));
    let changed = current != entries;
    // Cloud Storage lists the ACL re-derived from a policy in entity order, even when the grants
    // are the ones the default ACL already had (recorded 2026-10-31), so a different stored
    // order is rewritten without counting as a change.
    if !changed && stored_order == entries {
        mark_bound(connection, bucket)?;
        return Ok(false);
    }
    let encoded = serde_json::to_string(&entries).map_err(unavailable)?;
    sql::exec(
        connection,
        UPSERT_BUCKET_ENTRIES,
        params![bucket, BucketScope::Bucket, encoded],
    )?;
    mark_bound(connection, bucket)?;
    Ok(changed)
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
    } else if let Some(entries) = stored_bucket_entries(connection, bucket, BucketScope::Bucket)? {
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
