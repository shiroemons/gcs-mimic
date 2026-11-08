//! The members a bucket entry writes. A start compares only these with an existing bucket, so a
//! member the entry omits stays whatever the API made it.

use crate::{
    auth::Policy,
    bucket::{BucketRecord, cors_value},
    storage::admin::initialization::{EntryDigest, Stored},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Separates this encoding from any other SHA-256 input. Changing what an entry digest covers
/// needs a new label: otherwise an old digest could match an entry that now means something else.
const DIGEST_LABEL: &[u8] = b"gcs-mimic bucket file entry, version 1\n";

/// A member that a start compares with an existing bucket, in the order diagnostics list them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Member {
    Location,
    StorageClass,
    Labels,
    Cors,
    Versioning,
    DefaultEventBasedHold,
    PublicAccessPrevention,
    SoftDeletePolicy,
    IamPolicy,
}

impl Member {
    /// The entry field that writes the member.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::Location => "location",
            Self::StorageClass => "storageClass",
            Self::Labels => "labels",
            Self::Cors => "cors",
            Self::Versioning => "versioning.enabled",
            Self::DefaultEventBasedHold => "defaultEventBasedHold",
            Self::PublicAccessPrevention => "iamConfiguration.publicAccessPrevention",
            Self::SoftDeletePolicy => "softDeletePolicy.retentionDurationSeconds",
            Self::IamPolicy => "iamPolicy",
        }
    }

    /// The member's value in the bucket an insert would create, normalized as stored, so that
    /// spellings with one meaning, such as `"us"` and `"US"`, give one digest.
    fn value(self, record: &BucketRecord, policy: Option<&Policy>) -> Value {
        match self {
            Self::Location => record.location.as_str().into(),
            Self::StorageClass => record.storage_class.as_str().into(),
            Self::Labels => Value::Object(
                record
                    .labels
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_str().into()))
                    .collect(),
            ),
            Self::Cors => cors_value(&record.cors),
            Self::Versioning => record.versioning_enabled.into(),
            Self::DefaultEventBasedHold => record.default_event_based_hold.into(),
            Self::PublicAccessPrevention => record.public_access_prevention_enforced.into(),
            Self::SoftDeletePolicy => retention(record).into(),
            Self::IamPolicy => policy.map_or(Value::Null, |policy| {
                Value::Object(
                    grants(policy)
                        .into_iter()
                        .map(|(role, members)| {
                            (role.to_owned(), members.into_iter().collect::<Value>())
                        })
                        .collect(),
                )
            }),
        }
    }

    /// Whether the existing bucket holds another value than the entry writes.
    fn differs(self, wanted: &BucketRecord, policy: Option<&Policy>, stored: &Stored) -> bool {
        let record = &stored.record;
        match self {
            Self::Location => wanted.location != record.location,
            Self::StorageClass => wanted.storage_class != record.storage_class,
            Self::Labels => wanted.labels != record.labels,
            Self::Cors => wanted.cors != record.cors,
            // A written `enabled` also makes the setting visible, which a bucket without one lacks.
            Self::Versioning => {
                (wanted.versioning_enabled, wanted.versioning_configured)
                    != (record.versioning_enabled, record.versioning_configured)
            }
            Self::DefaultEventBasedHold => {
                wanted.default_event_based_hold != record.default_event_based_hold
            }
            Self::PublicAccessPrevention => {
                wanted.public_access_prevention_enforced != record.public_access_prevention_enforced
            }
            Self::SoftDeletePolicy => retention(wanted) != retention(record),
            Self::IamPolicy => policy.map(grants) != Some(grants(&stored.policy)),
        }
    }
}

fn retention(record: &BucketRecord) -> Option<u64> {
    record
        .soft_delete_policy
        .as_ref()
        .map(|policy| policy.retention_duration_seconds)
}

/// What a policy grants: binding order and repeated members do not change who may do what.
fn grants(policy: &Policy) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut grants: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for binding in &policy.bindings {
        grants
            .entry(binding.role.as_str())
            .or_default()
            .extend(binding.members.iter().map(String::as_str));
    }
    grants
}

/// The members one entry writes.
#[derive(Debug, Default)]
pub(crate) struct Written(BTreeSet<Member>);

impl FromIterator<Member> for Written {
    fn from_iter<I: IntoIterator<Item = Member>>(members: I) -> Self {
        Self(members.into_iter().collect())
    }
}

impl Written {
    /// Digests the written members and their normalized values, leaving out `name` and `project`:
    /// the name selects the bucket, and the project is ownership, which the run compares on its own
    /// and never changes.
    pub(crate) fn digest(&self, record: &BucketRecord, policy: Option<&Policy>) -> EntryDigest {
        // Sorted by field name, so the digest does not depend on the order of `Member`.
        let sorted: BTreeMap<_, _> = self
            .0
            .iter()
            .map(|member| (member.field(), member.value(record, policy)))
            .collect();
        let canonical = Value::Object(
            sorted
                .into_iter()
                .map(|(field, value)| (field.to_owned(), value))
                .collect(),
        );
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        context.update(DIGEST_LABEL);
        context.update(canonical.to_string().as_bytes());
        let mut bytes = [0; 32];
        bytes.copy_from_slice(context.finish().as_ref());
        EntryDigest::new(bytes)
    }

    /// The written members whose value the existing bucket does not hold.
    pub(crate) fn differences<'a>(
        &'a self,
        wanted: &'a BucketRecord,
        policy: Option<&'a Policy>,
        stored: &'a Stored,
    ) -> impl Iterator<Item = Member> + 'a {
        self.0
            .iter()
            .copied()
            .filter(move |member| member.differs(wanted, policy, stored))
    }
}
