//! Pure JSON bucket and object API building blocks; no catalog, authorization or socket access.
//!
//! [`route`] parses raw paths and queries into [`BucketRequest`]; other routes return `None`.
//! [`read_body`] bounds collection before [`parse_insert`] creates a shared bucket record.
//! The caller performs catalog operations and atomic condition checks, then uses
//! [`bucket_resource`] / [`bucket_list`] with its advertised endpoint to render metadata.
//! [`encode_page_token`] creates project/prefix-bound continuation cursors.
//! [`JsonError`] maps catalog failures and produces JSON HTTP error responses.
//! The supported subset and deviations are recorded in `docs/en/slices/json-bucket-crud.md`.

mod body;
mod buckets;
mod error;
mod iam;
mod multipart;
mod object_list;
mod object_metadata;
mod objects;
mod page_token;
mod query;
pub(crate) mod resumable;

pub use body::read_body;
pub(crate) use buckets::timestamp;
pub use buckets::{bucket_list, bucket_resource, parse_insert, validate_bucket_name};
pub use error::JsonError;
pub(crate) use iam::policy_etag;
pub use iam::{parse_policy, policy_resource};
pub use multipart::{Multipart, multipart_boundary, multipart_metadata};
pub use object_list::{ObjectListParams, object_list};
pub use object_metadata::{ObjectMetadataRequest, parse_object_metadata};
pub use objects::{
    ObjectGetParams, ObjectInsertParams, ObjectRequest, object_media_headers, object_media_range,
    object_resource, parse_object_upload_headers, route_object, validate_object_get_headers,
    validate_object_media_encoding, validate_object_name,
};
pub use page_token::{decode_page_token, encode_page_token};
pub use query::{
    BucketRequest, Conditions, GetParams, InsertParams, ListParams, Projection, route,
    validate_project,
};
pub(crate) use query::{decode, parse_query};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod object_tests;
