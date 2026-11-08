use super::{Identity, Permission, roles};
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) enum PolicyDecodeError {
    Json(serde_json::Error),
    Shape(&'static str),
}

impl std::fmt::Display for PolicyDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid stored policy: {error}"),
            Self::Shape(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PolicyDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Shape(_) => None,
        }
    }
}

impl From<PolicyDecodeError> for crate::storage::failure::BackendCause {
    fn from(error: PolicyDecodeError) -> Self {
        Self::from_error(error)
    }
}

/// Whether `allUsers` and `allAuthenticatedUsers` bindings may grant access to a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicAccess {
    Allowed,
    /// Public access prevention is enforced on the bucket or its parent project.
    Prevented,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub role: String,
    pub members: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub bindings: Vec<Binding>,
    pub etag: Option<String>,
}

impl Policy {
    pub(crate) fn default_for(project: &str) -> Self {
        Self {
            bindings: vec![
                Binding {
                    role: "roles/storage.legacyBucketOwner".into(),
                    members: vec![
                        format!("projectEditor:{project}"),
                        format!("projectOwner:{project}"),
                    ],
                },
                Binding {
                    role: "roles/storage.legacyBucketReader".into(),
                    members: vec![format!("projectViewer:{project}")],
                },
            ],
            etag: None,
        }
    }

    pub(crate) fn allows(
        &self,
        identity: Option<&Identity>,
        permission: Permission,
        public_access: PublicAccess,
    ) -> bool {
        self.bindings.iter().any(|binding| {
            roles::permissions(&binding.role)
                .is_some_and(|permissions| permissions.binary_search(&permission.as_str()).is_ok())
                && binding.members.iter().any(|member| {
                    (matches!(public_access, PublicAccess::Allowed)
                        && (member == "allUsers"
                            || (member == "allAuthenticatedUsers" && identity.is_some())))
                        || identity.is_some_and(|identity| {
                            member
                                .strip_prefix("user:")
                                .or_else(|| member.strip_prefix("serviceAccount:"))
                                .is_some_and(|user| identity.matches_acl_user(user))
                        })
                })
        })
    }

    /// Whether any binding grants `allUsers` or `allAuthenticatedUsers`, which public access
    /// prevention refuses to store.
    pub(crate) fn names_public_principal(&self) -> bool {
        self.bindings.iter().any(|binding| {
            binding
                .members
                .iter()
                .any(|member| matches!(member.as_str(), "allUsers" | "allAuthenticatedUsers"))
        })
    }

    pub(crate) fn bindings_json(&self) -> Value {
        Value::Array(
            self.bindings
                .iter()
                .map(|binding| json!({"role": binding.role, "members": binding.members}))
                .collect(),
        )
    }

    pub(crate) fn from_stored(bindings: &str, etag: String) -> Result<Self, PolicyDecodeError> {
        use PolicyDecodeError::Shape;
        let value: Value = serde_json::from_str(bindings).map_err(PolicyDecodeError::Json)?;
        let bindings = value
            .as_array()
            .ok_or(Shape("invalid stored bindings"))?
            .iter()
            .map(|binding| -> Result<Binding, PolicyDecodeError> {
                Ok(Binding {
                    role: binding["role"]
                        .as_str()
                        .ok_or(Shape("invalid stored role"))?
                        .to_owned(),
                    members: binding["members"]
                        .as_array()
                        .ok_or(Shape("invalid stored members"))?
                        .iter()
                        .map(|member| {
                            member
                                .as_str()
                                .map(str::to_owned)
                                .ok_or(Shape("invalid stored member"))
                        })
                        .collect::<Result<_, _>>()?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            bindings,
            etag: Some(etag),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_policy_retains_the_json_cause_through_catalog_conversion() {
        let error = Policy::from_stored("{", "etag".into()).unwrap_err();
        let cause: crate::storage::failure::BackendCause = error.into();
        let error = crate::bucket::CatalogError::Corrupt(cause);
        let mut source: &(dyn std::error::Error + 'static) = &error;
        while !source.is::<serde_json::Error>() {
            source = source
                .source()
                .expect("JSON cause must remain in the chain");
        }
    }
}
