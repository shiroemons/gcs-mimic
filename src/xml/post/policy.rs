//! Form policy versions have distinct credential fields and condition exemptions.

use super::Error;
use std::collections::BTreeMap;

pub(super) const POLICY: &str = "policy";
const GOOGLE_ACCESS_ID: &str = "googleaccessid";
const V2_SIGNATURE: &str = "signature";
const ALGORITHM: &str = "x-goog-algorithm";
const CREDENTIAL: &str = "x-goog-credential";
const DATE: &str = "x-goog-date";
const V4_SIGNATURE: &str = "x-goog-signature";
pub(super) const V2_FIELDS: [&str; 2] = [GOOGLE_ACCESS_ID, V2_SIGNATURE];
pub(super) const V4_FIELDS: [&str; 4] = [ALGORITHM, CREDENTIAL, DATE, V4_SIGNATURE];

/// The credentials a policy form carries, read once its version is chosen.
pub(super) enum Credentials<'f> {
    V2 {
        access_id: &'f str,
        signature: &'f str,
    },
    V4 {
        algorithm: &'f str,
        credential: &'f str,
        date: &'f str,
        signature: &'f str,
    },
}

/// A form's base64 policy document and its credentials.
pub(super) struct PolicyForm<'f> {
    pub(super) document: &'f str,
    pub(super) credentials: Credentials<'f>,
}

impl<'f> PolicyForm<'f> {
    /// `None` for a form without a `policy` field, which authenticates by its `Authorization`
    /// header instead. Credentials of both versions, or credentials without a policy, are
    /// refused first; then each required field is checked in its version's order.
    pub(super) fn select(fields: &'f BTreeMap<String, String>) -> Result<Option<Self>, Error> {
        let has_v2 = V2_FIELDS.iter().any(|name| fields.contains_key(*name));
        let has_v4 = V4_FIELDS.iter().any(|name| fields.contains_key(*name));
        if has_v2 && has_v4 {
            return Err(Error::invalid("Use exactly one policy signing version."));
        }
        let Some(document) = fields.get(POLICY) else {
            if has_v2 || has_v4 {
                return Err(Error::invalid("Policy credentials require a policy field."));
            }
            return Ok(None);
        };
        let required = |name: &'static str| {
            fields
                .get(name)
                .filter(|value| !value.is_empty())
                .map(String::as_str)
                .ok_or_else(|| {
                    Error::invalid(format!("Form field {name} is required with policy."))
                })
        };
        // Struct fields evaluate in source order, which keeps the reported missing field.
        let credentials = if has_v2 {
            Credentials::V2 {
                access_id: required(GOOGLE_ACCESS_ID)?,
                signature: required(V2_SIGNATURE)?,
            }
        } else {
            Credentials::V4 {
                algorithm: required(ALGORITHM)?,
                credential: required(CREDENTIAL)?,
                date: required(DATE)?,
                signature: required(V4_SIGNATURE)?,
            }
        };
        Ok(Some(Self {
            document,
            credentials,
        }))
    }
}

impl Credentials<'_> {
    /// Whether a form field needs no policy condition: the policy and the signature, and for
    /// V2 the access ID as well.
    pub(super) fn unconditioned(&self, field: &str) -> bool {
        field == POLICY
            || match self {
                Self::V2 { .. } => V2_FIELDS.contains(&field),
                Self::V4 { .. } => field == V4_SIGNATURE,
            }
    }

    /// Only V4 requires a condition that binds the bucket.
    pub(super) fn requires_bucket_condition(&self) -> bool {
        matches!(self, Self::V4 { .. })
    }
}
