//! Pure JSON bucket API building blocks; no catalog, authorization or socket access.
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
mod page_token;
mod query;

pub use body::read_body;
pub use buckets::{bucket_list, bucket_resource, parse_insert, validate_bucket_name};
pub use error::JsonError;
pub use page_token::{decode_page_token, encode_page_token};
pub use query::{
    BucketRequest, Conditions, GetParams, InsertParams, ListParams, Projection, route,
};

#[cfg(test)]
mod tests;
