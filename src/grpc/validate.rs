use crate::object::ObjectConditions;
use tonic::Status;

macro_rules! request_conditions {
    ($validate:path, $request:ident) => {
        $validate(
            $request.if_generation_match,
            $request.if_generation_not_match,
            $request.if_metageneration_match,
            $request.if_metageneration_not_match,
        )
    };
    ($validate:path, source $request:ident) => {
        $validate(
            $request.if_source_generation_match,
            $request.if_source_generation_not_match,
            $request.if_source_metageneration_match,
            $request.if_source_metageneration_not_match,
        )
    };
}
pub(super) use request_conditions;

pub(super) fn lenient_conditions(
    g: Option<i64>,
    ng: Option<i64>,
    m: Option<i64>,
    nm: Option<i64>,
    message: &str,
) -> Result<ObjectConditions, Status> {
    if [g, ng, m, nm].into_iter().flatten().any(|v| v < 0) {
        return Err(Status::invalid_argument(message));
    }
    Ok(ObjectConditions {
        if_generation_match: g,
        if_generation_not_match: ng,
        if_metageneration_match: m,
        if_metageneration_not_match: nm,
    })
}

pub(super) fn strict_conditions(
    g: Option<i64>,
    ng: Option<i64>,
    m: Option<i64>,
    nm: Option<i64>,
) -> Result<ObjectConditions, Status> {
    const MESSAGE: &str = "Invalid generation conditions; use one match or not-match per generation, for example if_generation_match=0";
    let conditions = lenient_conditions(g, ng, m, nm, MESSAGE)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_keep_the_strict_and_lenient_subsets() {
        for values in [
            (None, None, Some(0), None),
            (None, None, None, Some(0)),
            (Some(1), Some(2), None, None),
            (None, None, Some(1), Some(2)),
        ] {
            let (g, ng, m, nm) = values;
            assert!(lenient_conditions(g, ng, m, nm, "lenient").is_ok());
            assert_eq!(
                strict_conditions(g, ng, m, nm).unwrap_err().code(),
                tonic::Code::InvalidArgument
            );
        }
        assert!(strict_conditions(Some(0), None, Some(1), None).is_ok());
        assert_eq!(
            lenient_conditions(Some(-1), None, None, None, "lenient")
                .unwrap_err()
                .message(),
            "lenient"
        );
    }
}
