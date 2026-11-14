use crate::{
    bucket::BucketConditions,
    object::{ExpectedHashes, ObjectConditions},
};
use tonic::Status;

pub(super) fn lenient_conditions(
    conditions: ObjectConditions,
    message: &str,
) -> Result<ObjectConditions, Status> {
    let ObjectConditions {
        if_generation_match: g,
        if_generation_not_match: ng,
        if_metageneration_match: m,
        if_metageneration_not_match: nm,
    } = conditions;
    if [g, ng, m, nm].into_iter().flatten().any(|v| v < 0) {
        return Err(Status::invalid_argument(message));
    }
    Ok(conditions)
}

pub(super) fn strict_conditions(conditions: ObjectConditions) -> Result<ObjectConditions, Status> {
    const MESSAGE: &str = "Invalid generation conditions; use one match or not-match per generation, for example if_generation_match=0";
    let conditions = lenient_conditions(conditions, MESSAGE)?;
    let ObjectConditions {
        if_generation_match: g,
        if_generation_not_match: ng,
        if_metageneration_match: m,
        if_metageneration_not_match: nm,
    } = conditions;
    if m == Some(0)
        || nm == Some(0)
        || (g.is_some() && ng.is_some())
        || (m.is_some() && nm.is_some())
    {
        return Err(Status::invalid_argument(MESSAGE));
    }
    Ok(conditions)
}

/// Bucket metageneration conditions under the object rule, whose wording they share today.
pub(super) fn lenient_bucket_conditions(
    conditions: BucketConditions,
    message: &str,
) -> Result<BucketConditions, Status> {
    lenient_conditions(metageneration_only(conditions), message).map(|_| conditions)
}

/// Bucket metageneration conditions under [`strict_conditions`].
pub(super) fn strict_bucket_conditions(
    conditions: BucketConditions,
) -> Result<BucketConditions, Status> {
    strict_conditions(metageneration_only(conditions)).map(|_| conditions)
}

fn metageneration_only(conditions: BucketConditions) -> ObjectConditions {
    let BucketConditions {
        if_metageneration_match,
        if_metageneration_not_match,
    } = conditions;
    ObjectConditions {
        if_metageneration_match,
        if_metageneration_not_match,
        ..ObjectConditions::default()
    }
}

pub(super) fn generation(value: i64, message: &str) -> Result<Option<i64>, Status> {
    if value < 0 {
        return Err(Status::invalid_argument(message));
    }
    Ok((value != 0).then_some(value))
}

pub(super) fn common<T: Default + PartialEq>(
    params: Option<&T>,
    message: &str,
) -> Result<(), Status> {
    if params.is_some_and(|p| p != &T::default()) {
        return Err(super::v2::status::unsupported(message));
    }
    Ok(())
}

pub(super) fn merge_hashes(
    first: ExpectedHashes,
    next: ExpectedHashes,
    message: &str,
) -> Result<ExpectedHashes, Status> {
    super::upload::merge_hashes(first, next).map_err(|_| Status::invalid_argument(message))
}

pub(super) fn resolve_range(size: u64, offset: i64, limit: i64) -> Option<(u64, u64)> {
    let limit = u64::try_from(limit).ok()?;
    let start = if offset < 0 {
        size.saturating_sub(offset.unsigned_abs())
    } else {
        u64::try_from(offset).ok()?
    };
    let remaining = size.checked_sub(start)?;
    Some((
        start,
        if limit == 0 {
            remaining
        } else {
            remaining.min(limit)
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::test_support::assert_code;

    #[test]
    fn conditions_keep_the_strict_and_lenient_subsets() {
        for values in [
            (None, None, Some(0), None),
            (None, None, None, Some(0)),
            (Some(1), Some(2), None, None),
            (None, None, Some(1), Some(2)),
        ] {
            let (g, ng, m, nm) = values;
            let conditions = ObjectConditions {
                if_generation_match: g,
                if_generation_not_match: ng,
                if_metageneration_match: m,
                if_metageneration_not_match: nm,
            };
            assert!(lenient_conditions(conditions, "lenient").is_ok());
            assert_code(strict_conditions(conditions), tonic::Code::InvalidArgument);
        }
        assert!(
            strict_conditions(ObjectConditions {
                if_generation_match: Some(0),
                if_metageneration_match: Some(1),
                ..Default::default()
            })
            .is_ok()
        );
        assert_eq!(
            lenient_conditions(
                ObjectConditions {
                    if_generation_match: Some(-1),
                    ..Default::default()
                },
                "lenient"
            )
            .unwrap_err()
            .message(),
            "lenient"
        );
    }

    #[test]
    fn bucket_conditions_keep_the_strict_and_lenient_subsets() {
        let both = BucketConditions {
            if_metageneration_match: Some(1),
            if_metageneration_not_match: Some(2),
        };
        assert_eq!(lenient_bucket_conditions(both, "lenient").unwrap(), both);
        for rejected in [
            both,
            BucketConditions {
                if_metageneration_match: Some(0),
                ..Default::default()
            },
            BucketConditions {
                if_metageneration_not_match: Some(0),
                ..Default::default()
            },
            BucketConditions {
                if_metageneration_match: Some(-1),
                ..Default::default()
            },
        ] {
            assert_code(
                strict_bucket_conditions(rejected),
                tonic::Code::InvalidArgument,
            );
        }
        let one = BucketConditions {
            if_metageneration_match: Some(1),
            ..Default::default()
        };
        assert_eq!(strict_bucket_conditions(one).unwrap(), one);
        assert_eq!(
            lenient_bucket_conditions(
                BucketConditions {
                    if_metageneration_not_match: Some(-1),
                    ..Default::default()
                },
                "lenient"
            )
            .unwrap_err()
            .message(),
            "lenient"
        );
    }
}
