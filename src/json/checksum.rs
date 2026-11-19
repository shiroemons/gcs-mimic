//! Declared object digests carried by the `md5Hash`/`crc32c` resource fields. The `X-Goog-Hash`
//! header is read by `crate::object::upload_headers`.

use super::JsonError;
use crate::object::upload_headers::decode;
use crate::object::{Checksum, HashAlgorithm};
use serde_json::{Map, Value};

/// A resource digest field whose value is not a string.
pub(super) const NOT_BASE64: &str = "Checksum must be a base64 string";

/// Removes `md5Hash`, then `crc32c`, from a resource and passes each decoded digest to
/// `declare`. A value that is not a string fails with `not_a_string`.
pub(super) fn take_resource_hashes(
    resource: &mut Map<String, Value>,
    not_a_string: fn() -> JsonError,
    mut declare: impl FnMut(Checksum) -> Result<(), JsonError>,
) -> Result<(), JsonError> {
    for (algorithm, field) in [
        (HashAlgorithm::Md5, "md5Hash"),
        (HashAlgorithm::Crc32c, "crc32c"),
    ] {
        if let Some(value) = resource.remove(field) {
            let text = value.as_str().ok_or_else(not_a_string)?;
            declare(decode(algorithm, text)?)?;
        }
    }
    Ok(())
}
