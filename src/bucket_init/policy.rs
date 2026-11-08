//! The `iamPolicy` member of a bucket entry: the bindings a new bucket starts with, checked by the
//! same role, member and occurrence rules as `setIamPolicy`.
//!
//! Diagnostics never repeat a role or member: like every other initializer diagnostic they name
//! the field and the fix, so a mistyped principal cannot leak into logs.

use super::{Collected, Members, Reason, array, closed_with, error, members, path, value};
use crate::{
    auth::{
        Binding, Policy,
        policy_input::{self, PolicyInput, PolicyInputError},
    },
    diagnostics::{DiagnosticSource, Diagnostics},
};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};

pub(super) const EXAMPLE: &str = "\"iamPolicy\":{\"bindings\":[{\"role\":\"roles/storage.objectViewer\",\"members\":[\"allUsers\"]}]}";
const MEMBER_FIX: &str = "Invalid member; use allUsers, allAuthenticatedUsers, user:EMAIL, serviceAccount:EMAIL or projectViewer:PROJECT.";

/// Only the grants: the catalog owns a policy's version and etag, so a file cannot state them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IamPolicy {
    bindings: Vec<PolicyBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyBinding {
    role: String,
    members: Vec<String>,
}

impl IamPolicy {
    /// The policy this entry stores. Validation has already accepted it, so a failure here is a
    /// broken invariant reported against the entry rather than a panic.
    pub(super) fn into_policy(self, key: &str) -> Result<Policy, Diagnostics> {
        PolicyInput {
            version: 1,
            bindings: self
                .bindings
                .into_iter()
                .map(|binding| Binding {
                    role: binding.role,
                    members: binding.members,
                })
                .collect(),
            etag: None,
        }
        .into_policy()
        .map_err(|_| {
            Diagnostics::error(
                DiagnosticSource::Initializer,
                key,
                "Invalid IAM policy; correct its roles and members.",
            )
        })
    }
}

fn policy_unsupported(name: &str) -> Option<&'static str> {
    matches!(name, "version" | "etag" | "auditConfigs").then_some(
        "Unsupported in a bucket initialization policy; write only its bindings, for example \"iamPolicy\":{\"bindings\":[]}. The catalog keeps the version and etag.",
    )
}

fn binding_unsupported(name: &str) -> Option<&'static str> {
    (name == "condition").then_some("Conditional role bindings are unsupported; remove condition.")
}

fn find<'a>(fields: &Members<'a>, name: &str) -> Option<&'a RawValue> {
    fields
        .0
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, raw)| *raw)
}

/// Checks member names; values are checked by [`check_values`].
pub(super) fn check_shape(raw: &RawValue, key: &str, diagnostics: &mut Collected) {
    let Some(policy) = members(raw) else {
        return;
    };
    closed_with::<IamPolicy>(&policy, key, diagnostics, policy_unsupported);
    let bindings_key = path(key, "bindings");
    let Some(bindings) = find(&policy, "bindings") else {
        error(
            diagnostics,
            Reason::Shape,
            bindings_key,
            format!("Missing bindings; use {EXAMPLE}, or \"bindings\":[] for no grants."),
        );
        return;
    };
    for (index, raw) in array(bindings).unwrap_or_default().iter().enumerate() {
        let Some(binding) = members(raw) else {
            continue;
        };
        let binding_key = format!("{bindings_key}[{index}]");
        closed_with::<PolicyBinding>(&binding, &binding_key, diagnostics, binding_unsupported);
        for field in ["role", "members"] {
            if find(&binding, field).is_none() {
                error(
                    diagnostics,
                    Reason::Shape,
                    path(&binding_key, field),
                    format!("Missing {field}; use {}.", super::example(field)),
                );
            }
        }
    }
}

/// Checks each role and member as `setIamPolicy` would, reporting the narrowest key.
pub(super) fn check_values(
    raw: &RawValue,
    key: &str,
    diagnostics: &mut Collected,
) -> Result<(), Diagnostics> {
    let Some(policy) = members(raw) else {
        error(
            diagnostics,
            Reason::Native,
            key,
            format!("Expected a policy object; use {EXAMPLE}."),
        );
        return Ok(());
    };
    let mut principals = 0usize;
    for (_, raw) in policy.0.iter().filter(|(name, _)| name == "bindings") {
        let bindings_key = path(key, "bindings");
        let Some(bindings) = array(raw) else {
            error(
                diagnostics,
                Reason::Native,
                bindings_key,
                format!("Expected an array of bindings; use {EXAMPLE}."),
            );
            continue;
        };
        for (index, raw) in bindings.iter().enumerate() {
            let binding_key = format!("{bindings_key}[{index}]");
            let Some(binding) = members(raw) else {
                error(
                    diagnostics,
                    Reason::Native,
                    binding_key,
                    "Expected a binding object; use {\"role\":\"roles/storage.objectViewer\",\"members\":[\"allUsers\"]}.",
                );
                continue;
            };
            for (field, raw) in &binding.0 {
                let field_key = path(&binding_key, field);
                match field.as_str() {
                    "role" => check_role(&value(raw)?, &field_key, diagnostics),
                    "members" => principals += check_members(raw, &field_key, diagnostics)?,
                    _ => {}
                }
            }
        }
    }
    if policy_input::validate_principal_count(principals).is_err() {
        error(
            diagnostics,
            Reason::Limit,
            key,
            "A policy names at most 1500 principals across its bindings, counting repeats; remove some members.",
        );
    }
    Ok(())
}

fn check_role(role: &Value, key: &str, diagnostics: &mut Collected) {
    let (reason, fix) = match role.as_str().map(policy_input::validate_role) {
        Some(Ok(())) => return,
        Some(Err(PolicyInputError::UnsupportedInput(_))) => (
            Reason::Unsupported,
            "Basic and custom roles are unsupported; use a predefined Cloud Storage role, for example \"role\":\"roles/storage.objectViewer\".",
        ),
        Some(Err(PolicyInputError::InvalidInput(_))) | None => (
            Reason::Native,
            "Not a role that can be granted on a bucket; use a predefined Cloud Storage role, for example \"role\":\"roles/storage.objectViewer\".",
        ),
    };
    error(diagnostics, reason, key, fix);
}

/// Returns how many principals the array names, valid or not, for the policy's occurrence limit.
fn check_members(
    raw: &RawValue,
    key: &str,
    diagnostics: &mut Collected,
) -> Result<usize, Diagnostics> {
    let Some(items) = array(raw) else {
        error(
            diagnostics,
            Reason::Native,
            key,
            "Expected an array of members; use \"members\":[\"allUsers\"].",
        );
        return Ok(0);
    };
    for (index, raw) in items.iter().enumerate() {
        let (reason, fix) = match value(raw)?.as_str().map(policy_input::validate_member) {
            Some(Ok(())) => continue,
            Some(Err(PolicyInputError::UnsupportedInput(_))) => (
                Reason::Unsupported,
                "Groups, domains, deleted and federated principals are unsupported; use allUsers, allAuthenticatedUsers, user:EMAIL, serviceAccount:EMAIL or projectViewer:PROJECT.",
            ),
            Some(Err(PolicyInputError::InvalidInput(_))) | None => (Reason::Native, MEMBER_FIX),
        };
        error(diagnostics, reason, format!("{key}[{index}]"), fix);
    }
    Ok(items.len())
}
