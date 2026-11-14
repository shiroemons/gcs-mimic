//! The members a bucket entry writes. A start compares only these with an existing bucket and
//! reconcile mode applies only these, so a member the entry omits stays whatever the API made it.

use crate::{
    auth::Policy,
    bucket::{BucketMetadataUpdate, BucketRecord, SoftDeleteRetention, cors_value},
    object::MapPatch,
    storage::admin::{
        AdminError,
        initialization::{EntryDigest, MemberBits, RecordedEntry, Stored},
    },
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Separates this encoding from any other SHA-256 input. Changing what an entry digest covers
/// needs a new label: otherwise an old digest could match an entry that now means something else.
const DIGEST_LABEL: &[u8] = b"gcs-mimic bucket file entry, version 1\n";

#[cfg(test)]
mod tests;

/// Declares `Member` together with the list of every member, so decoding a recorded set cannot
/// miss one.
macro_rules! members {
    ($($(#[$meta:meta])* $member:ident),+ $(,)?) => {
        /// A member that a start compares with an existing bucket, in the order diagnostics list
        /// them.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub(crate) enum Member {
            $($(#[$meta])* $member),+
        }

        impl Member {
            const ALL: &'static [Self] = &[$(Self::$member),+];
        }
    };
}

members! {
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
    /// The member's bit in a recorded entry. The catalog keeps these bits, so a member keeps its
    /// bit for good and a new member takes an unused one.
    const fn bit(self) -> u32 {
        match self {
            Self::Location => 1 << 0,
            Self::StorageClass => 1 << 1,
            Self::Labels => 1 << 2,
            Self::Cors => 1 << 3,
            Self::Versioning => 1 << 4,
            Self::DefaultEventBasedHold => 1 << 5,
            Self::PublicAccessPrevention => 1 << 6,
            Self::SoftDeletePolicy => 1 << 7,
            Self::IamPolicy => 1 << 8,
        }
    }

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

    /// Sets the member in a PATCH to the value the entry writes, as `parse_bucket_patch` would
    /// read it, except that labels replace the whole map. Returns whether the PATCH carries it:
    /// the location never changes and the policy has a write of its own.
    fn patch(
        self,
        wanted: &BucketRecord,
        update: &mut BucketMetadataUpdate,
    ) -> Result<bool, AdminError> {
        match self {
            Self::Location | Self::IamPolicy => return Ok(false),
            Self::StorageClass => update.storage_class = Some(wanted.storage_class),
            Self::Labels => update.labels = MapPatch::Replace(wanted.labels.clone()),
            Self::Cors => update.cors = Some(wanted.cors.clone()),
            Self::Versioning => {
                update.versioning_enabled = Some(wanted.versioning_enabled);
                update.versioning_configured = Some(wanted.versioning_configured);
            }
            Self::DefaultEventBasedHold => {
                update.default_event_based_hold = Some(wanted.default_event_based_hold);
            }
            Self::PublicAccessPrevention => {
                update.public_access_prevention_enforced =
                    Some(wanted.public_access_prevention_enforced);
            }
            Self::SoftDeletePolicy => {
                // Validation accepted the retention, so a failure here is a broken invariant.
                let retention = retention(wanted)
                    .and_then(|seconds| i64::try_from(seconds).ok())
                    .and_then(|seconds| SoftDeleteRetention::try_from(seconds).ok())
                    .ok_or_else(|| AdminError::Invalid("invalid soft delete retention".into()))?;
                update.soft_delete_retention = Some(retention);
            }
        }
        Ok(true)
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

/// The PATCH that writes `members` with the values of `wanted`, or `None` when no member is part
/// of a bucket update.
pub(crate) fn patch(
    members: &[Member],
    wanted: &BucketRecord,
) -> Result<Option<BucketMetadataUpdate>, AdminError> {
    let mut update = BucketMetadataUpdate::default();
    let mut carried = false;
    for member in members {
        carried |= member.patch(wanted, &mut update)?;
    }
    update.touched = carried;
    Ok(carried.then_some(update))
}

impl Written {
    pub(crate) fn contains(&self, member: Member) -> bool {
        self.0.contains(&member)
    }

    fn bits(&self) -> MemberBits {
        MemberBits::new(self.0.iter().fold(0, |bits, member| bits | member.bit()))
    }

    /// The entry a start records for a bucket that holds what this entry writes.
    pub(crate) fn record(&self, record: &BucketRecord, policy: Option<&Policy>) -> RecordedEntry {
        RecordedEntry {
            digest: self.digest(record, policy),
            members: self.bits(),
        }
    }

    /// The members a recorded entry wrote that this entry no longer writes.
    pub(crate) fn removed_since(&self, recorded: MemberBits) -> Vec<Member> {
        Member::ALL
            .iter()
            .copied()
            .filter(|member| recorded.get() & member.bit() != 0 && !self.contains(*member))
            .collect()
    }

    /// Digests the written members and their normalized values, leaving out `name` and `project`:
    /// the name selects the bucket, and the project is ownership, which the run compares on its own
    /// and never changes.
    fn digest(&self, record: &BucketRecord, policy: Option<&Policy>) -> EntryDigest {
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
