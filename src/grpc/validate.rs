use crate::{bucket::BucketConditions, object::ObjectConditions};
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

/// A byte range resolved against an object's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ByteRange {
    pub(super) start: u64,
    pub(super) len: u64,
}

/// Why a proto read range selects nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RangeError {
    NegativeLength,
    StartsPastEnd,
}

/// Resolves a proto read range: a negative `offset` counts back from the end and clamps at the
/// start, and a `limit` of 0 reads to the end. The limit is checked first.
pub(super) fn resolve_range(size: u64, offset: i64, limit: i64) -> Result<ByteRange, RangeError> {
    let limit = u64::try_from(limit).map_err(|_| RangeError::NegativeLength)?;
    let start =
        u64::try_from(offset).unwrap_or_else(|_| size.saturating_sub(offset.unsigned_abs()));
    let remaining = size.checked_sub(start).ok_or(RangeError::StartsPastEnd)?;
    let len = if limit == 0 {
        remaining
    } else {
        remaining.min(limit)
    };
    Ok(ByteRange { start, len })
}

/// The most items one list page returns; proto3's 0 asks for this many.
pub(super) const MAX_PAGE_SIZE: usize = 1000;

/// A list RPC's page size, or `None` for a negative one, which each API words differently.
pub(super) fn page_size(raw: i32) -> Option<usize> {
    let requested = usize::try_from(raw).ok()?;
    Some(if requested == 0 {
        MAX_PAGE_SIZE
    } else {
        requested.min(MAX_PAGE_SIZE)
    })
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
    fn read_ranges_resolve_against_the_object_size() {
        let range = |start, len| ByteRange { start, len };
        assert_eq!(resolve_range(15, 2, 0), Ok(range(2, 13)));
        assert_eq!(resolve_range(15, 2, 4), Ok(range(2, 4)));
        assert_eq!(resolve_range(15, 10, 9), Ok(range(10, 5)));
        assert_eq!(resolve_range(15, 15, 0), Ok(range(15, 0)));
        // A negative offset counts back from the end and clamps at the start.
        assert_eq!(resolve_range(15, -5, 3), Ok(range(10, 3)));
        assert_eq!(resolve_range(15, -20, 0), Ok(range(0, 15)));
        assert_eq!(resolve_range(15, i64::MIN, 0), Ok(range(0, 15)));
        assert_eq!(resolve_range(15, 16, 0), Err(RangeError::StartsPastEnd));
        assert_eq!(resolve_range(15, 0, -1), Err(RangeError::NegativeLength));
        // The limit is checked first.
        assert_eq!(resolve_range(15, 16, -1), Err(RangeError::NegativeLength));
    }

    #[test]
    fn page_sizes_default_and_cap_at_the_maximum() {
        assert_eq!(page_size(0), Some(MAX_PAGE_SIZE));
        assert_eq!(page_size(1), Some(1));
        assert_eq!(page_size(1000), Some(1000));
        assert_eq!(page_size(1001), Some(MAX_PAGE_SIZE));
        assert_eq!(page_size(i32::MAX), Some(MAX_PAGE_SIZE));
        assert_eq!(page_size(-1), None);
        assert_eq!(page_size(i32::MIN), None);
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
