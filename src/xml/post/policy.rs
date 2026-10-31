//! Form policy versions have distinct credential fields and condition exemptions.

use super::{Error, invalid};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Version {
    V2,
    V4,
}

impl Version {
    pub(super) fn select(fields: &BTreeMap<String, String>) -> Result<Option<Self>, Error> {
        let v2 = ["googleaccessid", "signature"];
        let v4 = [
            "x-goog-algorithm",
            "x-goog-credential",
            "x-goog-date",
            "x-goog-signature",
        ];
        let has_v2 = v2.iter().any(|name| fields.contains_key(*name));
        let has_v4 = v4.iter().any(|name| fields.contains_key(*name));
        if has_v2 && has_v4 {
            return Err(invalid("Use exactly one policy signing version."));
        }
        if !fields.contains_key("policy") {
            if has_v2 || has_v4 {
                return Err(invalid("Policy credentials require a policy field."));
            }
            return Ok(None);
        }
        let (version, required) = if has_v2 {
            (Self::V2, v2.as_slice())
        } else {
            (Self::V4, v4.as_slice())
        };
        for name in required {
            if fields.get(*name).is_none_or(String::is_empty) {
                return Err(invalid(format!(
                    "Form field {name} is required with policy."
                )));
            }
        }
        Ok(Some(version))
    }

    pub(super) fn unconditioned(self, field: &str) -> bool {
        match self {
            Self::V4 => matches!(field, "policy" | "x-goog-signature"),
            Self::V2 => matches!(field, "policy" | "googleaccessid" | "signature"),
        }
    }
}
