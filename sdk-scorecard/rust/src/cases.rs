//! The case list, in the Python probe's order. IDs and order are part of the
//! scorecard contract; `score.sh` compares them across SDKs.

mod bucket;
mod copy;
mod listing;
mod object;
mod retention;
mod signed;

use crate::harness::{Ctx, Result};
use std::{future::Future, pin::Pin};

pub(crate) type Probe = for<'a> fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

pub(crate) enum Case {
    Run(Probe),
    /// Decided statically from the SDK's public surface, never from a runtime result.
    Unsupported(&'static str),
}

macro_rules! run {
    ($probe:path) => {
        Case::Run(|ctx| Box::pin($probe(ctx)))
    };
}

pub(crate) const CASES: &[(&str, Case)] = &[
    ("bucket.create_get_delete", run!(bucket::create_get_delete)),
    (
        "bucket.list_prefix_paging",
        run!(bucket::list_prefix_paging),
    ),
    ("bucket.update_labels", run!(bucket::update_labels)),
    (
        "bucket.metageneration_precondition",
        run!(bucket::metageneration_precondition),
    ),
    (
        "bucket.versioning_history",
        run!(bucket::versioning_history),
    ),
    ("bucket.lifecycle", run!(bucket::lifecycle)),
    ("bucket.cors", run!(bucket::cors)),
    ("bucket.retention_policy", run!(retention::bucket_policy)),
    ("bucket.retention_lock", run!(retention::bucket_lock)),
    (
        "bucket.uniform_bucket_level_access",
        run!(bucket::uniform_bucket_level_access),
    ),
    (
        "bucket.public_access_prevention",
        run!(bucket::public_access_prevention),
    ),
    ("bucket.iam_policy", run!(bucket::iam_policy)),
    (
        "bucket.iam_test_permissions",
        run!(bucket::iam_test_permissions),
    ),
    (
        "bucket.soft_delete_policy",
        run!(bucket::soft_delete_policy),
    ),
    ("bucket.autoclass", run!(bucket::autoclass)),
    ("bucket.acl", run!(bucket::acl)),
    (
        "bucket.default_object_acl",
        run!(bucket::default_object_acl),
    ),
    (
        "bucket.notifications",
        Case::Unsupported(
            "google-cloud-storage 1.22.0 has no public API for bucket notification configurations",
        ),
    ),
    (
        "project.hmac_keys",
        Case::Unsupported("google-cloud-storage 1.22.0 has no public API for HMAC keys"),
    ),
    (
        "project.service_account",
        Case::Unsupported(
            "google-cloud-storage 1.22.0 has no public API for the project service account",
        ),
    ),
    ("object.write_read_small", run!(object::write_read_small)),
    ("object.resumable_large", run!(object::resumable_large)),
    ("object.metadata_on_write", run!(object::metadata_on_write)),
    ("object.checksum_rejected", run!(object::checksum_rejected)),
    ("object.md5_accepted", run!(object::md5_accepted)),
    ("object.range_reads", run!(object::range_reads)),
    ("object.update_metadata", run!(object::update_metadata)),
    ("object.delete_not_found", run!(object::delete_not_found)),
    (
        "object.does_not_exist_precondition",
        run!(object::does_not_exist_precondition),
    ),
    (
        "object.generation_match_precondition",
        run!(object::generation_match_precondition),
    ),
    (
        "object.metageneration_match_precondition",
        run!(object::metageneration_match_precondition),
    ),
    (
        "object.list_prefix_delimiter",
        run!(listing::prefix_delimiter),
    ),
    ("object.list_paging", run!(listing::paging)),
    ("object.list_offsets", run!(listing::offsets)),
    ("object.list_match_glob", run!(listing::match_glob)),
    ("object.copy_with_metadata", run!(copy::copy_with_metadata)),
    (
        "object.rewrite_storage_class",
        run!(copy::rewrite_storage_class),
    ),
    ("object.compose", run!(copy::compose)),
    ("object.move", run!(copy::move_object)),
    ("object.acl", run!(object::acl)),
    ("object.holds", run!(retention::holds)),
    ("object.retention", run!(retention::object_retention)),
    ("object.custom_time", run!(object::custom_time)),
    (
        "object.storage_class_on_write",
        run!(object::storage_class_on_write),
    ),
    ("object.gzip_transcoding", run!(object::gzip_transcoding)),
    (
        "object.customer_supplied_key",
        run!(object::customer_supplied_key),
    ),
    (
        "object.soft_delete_restore",
        run!(retention::soft_delete_restore),
    ),
    ("signed.v4_get", run!(signed::v4_get)),
    ("signed.v4_put", run!(signed::v4_put)),
    (
        "signed.v2_get",
        Case::Unsupported("google-cloud-storage 1.22.0 has no public API for V2 signed URLs"),
    ),
    ("signed.post_policy_v4", run!(signed::post_policy_v4)),
];

#[cfg(test)]
mod tests {
    use super::CASES;
    use std::collections::HashSet;

    #[test]
    fn case_list_matches_the_shared_scorecard_shape() {
        assert_eq!(CASES.len(), 51);
        let ids: HashSet<&str> = CASES.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), CASES.len(), "duplicate case IDs");
    }
}
