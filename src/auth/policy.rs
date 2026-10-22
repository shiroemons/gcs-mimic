use super::{Identity, roles};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub role: String,
    pub members: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
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

    pub(crate) fn allows(&self, identity: &Identity, permission: &str) -> bool {
        self.bindings.iter().any(|binding| {
            binding.members.iter().any(|member| {
                member
                    .strip_prefix("user:")
                    .or_else(|| member.strip_prefix("serviceAccount:"))
                    == Some(identity.id())
            }) && roles::PREDEFINED_ROLES.iter().any(|(role, permissions)| {
                *role == binding.role && permissions.binary_search(&permission).is_ok()
            })
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

    pub(crate) fn from_stored(bindings: &str, etag: String) -> Result<Self, &'static str> {
        let value: Value = serde_json::from_str(bindings).map_err(|_| "invalid stored policy")?;
        let bindings = value
            .as_array()
            .ok_or("invalid stored bindings")?
            .iter()
            .map(|binding| {
                Ok(Binding {
                    role: binding["role"]
                        .as_str()
                        .ok_or("invalid stored role")?
                        .to_owned(),
                    members: binding["members"]
                        .as_array()
                        .ok_or("invalid stored members")?
                        .iter()
                        .map(|member| {
                            member
                                .as_str()
                                .map(str::to_owned)
                                .ok_or("invalid stored member")
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
