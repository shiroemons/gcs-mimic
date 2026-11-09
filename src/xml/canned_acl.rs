//! `x-goog-acl` on object writes. The stored result is the JSON API's `predefinedAcl`; this
//! module adds the XML API's value names and error wording.

use super::{Error, put};
use crate::{
    acl::{InvalidCannedAcl, PredefinedObjectAcl},
    bucket::BucketAccess,
    json,
    object::ObjectError,
    server::ObjectCtx,
};
use hyper::HeaderMap;

pub(super) const HEADER: &str = "x-goog-acl";

/// Cloud Storage's 400 for a value it does not define (recorded 2026-11-09).
impl From<InvalidCannedAcl<'_>> for Error {
    fn from(error: InvalidCannedAcl<'_>) -> Self {
        Self::invalid_argument(error.to_string())
    }
}

pub(super) fn parse(headers: &HeaderMap) -> Result<Option<PredefinedObjectAcl>, Error> {
    let Some(value) = put::single(headers, HEADER)? else {
        return Ok(None);
    };
    Ok(Some(PredefinedObjectAcl::from_canned(value)?))
}

/// Refuses what the bucket cannot accept before any body is read. Publication rechecks uniform
/// access and public access prevention in its transaction, so a concurrent change cannot slip
/// past this answer.
pub(super) async fn check(
    requested: Option<PredefinedObjectAcl>,
    bucket: &str,
    access: &BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<(), Error> {
    if requested.is_none() {
        return Ok(());
    }
    // Documented for uploads: an anonymous object is owned by the project owners team, so an
    // anonymous caller could otherwise choose its object's ACL.
    if matches!(access, BucketAccess::Policy { identity: None, .. }) {
        return Err(Error::invalid(
            "Anonymous callers cannot specify x-goog-acl on upload; authenticate or omit x-goog-acl",
        ));
    }
    let record = ctx
        .state
        .catalog
        .get_bucket(bucket, &BucketAccess::Permissive)
        .await
        .map_err(json::JsonError::from)?
        .ok_or_else(|| put::storage_error(ObjectError::BucketNotFound, ctx.caller))?;
    if json::UniformAccess::of(record.uniform_bucket_level_access_locked_time, None)
        == json::UniformAccess::Enabled
    {
        return Err(Error::uniform_access_acl());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_values_answer_with_the_recorded_details() {
        for value in ["projectPrivate", "publicRead", "public-read-write", "bogus"] {
            let mut headers = HeaderMap::new();
            headers.insert(HEADER, value.parse().unwrap());
            let error = parse(&headers).unwrap_err();
            assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST, "{value}");
            assert_eq!(
                error.code,
                super::super::XmlCode::InvalidArgument,
                "{value}"
            );
            assert_eq!(error.message, "Invalid argument.", "{value}");
            assert_eq!(
                error.details.as_deref(),
                Some(format!("Invalid canned acl: {value}").as_str())
            );
        }
    }

    #[test]
    fn absent_and_known_values_parse_without_an_error() {
        assert_eq!(parse(&HeaderMap::new()).unwrap(), None);
        let mut headers = HeaderMap::new();
        headers.insert(HEADER, "bucket-owner-read".parse().unwrap());
        assert_eq!(
            parse(&headers).unwrap(),
            Some(PredefinedObjectAcl::BucketOwnerRead)
        );
    }

    #[test]
    fn repeated_values_are_refused_instead_of_choosing_one() {
        let mut headers = HeaderMap::new();
        headers.append(HEADER, "private".parse().unwrap());
        headers.append(HEADER, "public-read".parse().unwrap());
        assert_eq!(
            parse(&headers).unwrap_err().code,
            super::super::XmlCode::ExcessHeaderValues
        );
    }
}
