//! Declared object digests: the `X-Goog-Hash` header and the `md5Hash`/`crc32c` resource
//! fields. Each caller keeps its own rule for a repeated digest and its own wording, which
//! recorded answers and tests pin, so this module only reads the declarations.

use super::JsonError;
use crate::hashing::decode_hash;
use crate::object::{Checksum, HashAlgorithm};
use hyper::HeaderMap;
use serde_json::{Map, Value};

/// A resource digest field whose value is not a string.
pub(super) const NOT_BASE64: &str = "Checksum must be a base64 string";

/// Decodes a base64 digest of the algorithm's width.
pub(super) fn decode(algorithm: HashAlgorithm, text: &str) -> Result<Checksum, JsonError> {
    Ok(match algorithm {
        HashAlgorithm::Md5 => Checksum::Md5(decode_hash(text)?),
        HashAlgorithm::Crc32c => Checksum::Crc32c(u32::from_be_bytes(decode_hash(text)?)),
    })
}

/// Passes each `algorithm=value` entry of the `X-Goog-Hash` headers, in order, to `declare`. A
/// header that is not visible ASCII, or an entry without `=`, fails with `malformed`; an
/// algorithm other than `md5` and `crc32c` is unsupported.
pub(super) fn each_declared_hash(
    headers: &HeaderMap,
    malformed: &'static str,
    mut declare: impl FnMut(HashAlgorithm, &str) -> Result<(), JsonError>,
) -> Result<(), JsonError> {
    for header in headers.get_all("x-goog-hash") {
        let header = header
            .to_str()
            .map_err(|_| JsonError::invalid_parameter(malformed))?;
        for entry in header.split(',') {
            let (token, value) = entry
                .trim()
                .split_once('=')
                .ok_or_else(|| JsonError::invalid_parameter(malformed))?;
            let algorithm = match token {
                "md5" => HashAlgorithm::Md5,
                "crc32c" => HashAlgorithm::Crc32c,
                _ => return Err(JsonError::unsupported_object("x-goog-hash algorithm")),
            };
            declare(algorithm, value)?;
        }
    }
    Ok(())
}

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
