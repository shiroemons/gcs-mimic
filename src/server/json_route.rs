//! The JSON API operation a request names, parsed once per request.
//!
//! The route order lives only here. A second copy of it, such as one that work before dispatch
//! consulted, would drift: a route added to only one copy would be handled there as some other
//! operation.

use super::unsupported;
use crate::percent::RequestUri;
use crate::{
    config::Config,
    json::ApiError,
    json::{self, BucketRequest, Fields, ObjectRequest, ProjectRequest},
};
use hyper::Method;

/// A JSON API route and its parsed operation. A parse failure stays in the route, so that
/// dispatch reports it after authenticating the caller, as it does for every request.
pub(super) enum JsonRoute {
    Acl(Result<json::acl::AclRequest, ApiError>),
    /// A Google JSON API route that gcs-mimic refuses, named for the refusal message.
    Unsupported(&'static str),
    Notifications(Result<(json::notifications::NotificationRequest, Fields), ApiError>),
    /// A request on a resumable upload session URI.
    Session(Result<json::resumable::UploadSession, ApiError>),
    Rewrite(Result<json::rewrite::RewriteRequest, ApiError>),
    Object(Result<ObjectRequest, ApiError>),
    Project(Result<(ProjectRequest, Fields), ApiError>),
    Bucket(Result<BucketRequest, ApiError>),
}

impl JsonRoute {
    /// The route that claims `method` and `uri`. Routes overlap (an ACL or notification path is
    /// also below a bucket path), so the first match in this order wins, and this order is part
    /// of the wire contract.
    pub(super) fn parse(method: &Method, uri: &RequestUri, config: &Config) -> Option<Self> {
        let profile = config.compat_profile();
        json::acl::route_acl(method, uri, profile)
            .map(Self::Acl)
            .or_else(|| unsupported::route(method, uri).map(Self::Unsupported))
            .or_else(|| json::notifications::route(method, uri, profile).map(Self::Notifications))
            .or_else(|| json::resumable::session_route(method, uri, profile).map(Self::Session))
            .or_else(|| json::rewrite::route(method, uri, profile).map(Self::Rewrite))
            .or_else(|| json::route_object_with_profile(method, uri, profile).map(Self::Object))
            .or_else(|| json::route_project(method, uri, profile).map(Self::Project))
            .or_else(|| {
                json::route_with_profile(method, uri, profile, config.seed_project())
                    .map(Self::Bucket)
            })
    }
}
