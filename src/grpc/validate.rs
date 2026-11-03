use crate::object::{ExpectedHashes, ObjectConditions};
use tonic::Status;

macro_rules! request_conditions {
    ($validate:path, $request:ident) => {
        $validate($crate::grpc::validate::request_conditions!($request))
    };
    ($request:ident) => {
        $crate::object::ObjectConditions {
            if_generation_match: $request.if_generation_match,
            if_generation_not_match: $request.if_generation_not_match,
            if_metageneration_match: $request.if_metageneration_match,
            if_metageneration_not_match: $request.if_metageneration_not_match,
        }
    };
    ($validate:path, source $request:ident) => {
        $validate($crate::object::ObjectConditions {
            if_generation_match: $request.if_source_generation_match,
            if_generation_not_match: $request.if_source_generation_not_match,
            if_metageneration_match: $request.if_source_metageneration_match,
            if_metageneration_not_match: $request.if_source_metageneration_not_match,
        })
    };
}
pub(super) use request_conditions;

/// Read bucket fields by name so the two metageneration conditions cannot trade places.
macro_rules! bucket_conditions {
    ($request:ident) => {
        $crate::bucket::BucketConditions {
            if_metageneration_match: $request.if_metageneration_match,
            if_metageneration_not_match: $request.if_metageneration_not_match,
        }
    };
}
pub(super) use bucket_conditions;

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
    if matches!((first.crc32c, next.crc32c), (Some(a), Some(b)) if a != b)
        || matches!((first.md5, next.md5), (Some(a), Some(b)) if a != b)
    {
        return Err(Status::invalid_argument(message));
    }
    Ok(ExpectedHashes {
        crc32c: next.crc32c.or(first.crc32c),
        md5: next.md5.or(first.md5),
    })
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
}
