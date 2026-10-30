//! Pure JSON bucket and object API building blocks; no catalog, authorization or socket access.
//!
//! [`route`] parses raw paths and queries into [`BucketRequest`]; other routes return `None`.
//! [`read_body`] bounds collection before [`parse_insert`] creates a shared bucket record.
//! The caller performs catalog operations and atomic condition checks, then uses
//! [`bucket_resource`] / [`bucket_list`] with its advertised endpoint to render metadata.
//! [`encode_page_token`] creates project/prefix-bound continuation cursors.
//! [`JsonError`] maps catalog failures and produces JSON HTTP error responses.
//! The supported subset and deviations are recorded in `docs/en/slices/json-bucket-crud.md`.

pub(crate) mod acl;
mod body;
mod buckets;
pub mod compose;
pub(crate) mod cors;
mod csek;
mod derived_metadata;
mod error;
mod fields;
mod iam;
mod multipart;
pub(crate) mod notifications;
mod object_copy;
mod object_list;
mod object_metadata;
mod object_move;
mod object_restore;
mod object_retention;
mod objects;
mod page_token;
mod projects;
mod query;
pub(crate) mod resumable;
pub(crate) mod resumable_cancel;
pub(crate) mod rewrite;

pub(crate) use body::read_body;
pub use buckets::parse_insert;
pub(crate) use buckets::{
    PredefinedAclTarget, UniformAccess, bucket_etag, reject_predefined_acl, timestamp,
    validate_bucket_name_for_profile,
};
#[cfg(test)]
pub(crate) use buckets::{bucket_list, bucket_resource};
pub(crate) use buckets::{
    bucket_list_with_profile, bucket_resource_with_profile, parse_bucket_patch,
    parse_bucket_replacement, parse_insert_with_profile, validate_bucket_name,
};
pub(crate) use csek::{insert_response_headers, object_resource_keyed, request_key, source_key};
pub(crate) use derived_metadata::{DestinationLocation, derived_component_count, derived_metadata};
pub use error::JsonError;
pub(crate) use fields::Fields;
pub(crate) use iam::policy_etag;
pub(crate) use iam::{parse_policy, policy_resource, test_permissions_query};
pub(crate) use multipart::{
    Multipart, PartKind, form_data_boundary, multipart_boundary, multipart_metadata,
};
pub(crate) use object_copy::ObjectCopyParams;
pub(crate) use object_list::{ObjectListParams, object_list};
pub(crate) use object_metadata::parse_object_metadata_mutation;
pub(crate) use object_metadata::validate_fixed_metadata;
pub(crate) use object_metadata::{ObjectMetadataRequest, WriteMode, parse_object_metadata};
pub(crate) use object_move::ObjectMoveParams;
pub(crate) use objects::{
    EtagConditions, base64, decode_hash, object_etag, object_media_transcoding,
    validate_read_condition_headers,
};
pub(crate) use objects::{
    ObjectGetParams, ObjectInsertParams, ObjectRead, ObjectRequest, object_media_headers,
    object_media_range, object_resource, parse_object_upload_headers, route_object,
    route_object_with_profile, validate_keyed_object_headers, validate_object_get_headers,
    validate_object_media_encoding, validate_object_name,
};
pub(crate) use page_token::{
    decode_all_projects_page_token, decode_page_token, encode_all_projects_page_token,
    encode_page_token,
};
pub(crate) use projects::{
    ProjectRequest, hmac_key_list, hmac_key_metadata, hmac_key_resource, parse_hmac_key_update,
    route_project, service_account,
};
#[cfg(test)]
pub(crate) use query::route;
pub(crate) use query::{
    BucketRequest, Conditions, GetParams, InsertParams, ListParams, Projection, route_with_profile,
    validate_project,
};
pub(crate) use query::{Decoding, decode, parse_query};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod object_tests;
