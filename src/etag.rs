//! Entity tags of buckets, objects, ACL entries and IAM policies. Cloud Storage gives a
//! resource the same tag over the JSON API and gRPC, and the XML API quotes it for objects
//! without an MD5, so every adapter derives tags here.

use crate::{bucket::BucketRecord, object::ObjectRecord};

/// The validator of one object generation. It carries no content hash, so a keyless metadata
/// read of an encrypted object learns nothing about its plaintext.
pub(crate) fn object_etag(object: &ObjectRecord) -> String {
    revision_etag(Some(object.generation.get()), object.metageneration.get())
}

/// Cloud Storage encodes only the metageneration, so by inference from the encoding a recreated
/// bucket repeats its etag; that repetition itself has not been observed.
pub(crate) fn bucket_etag(bucket: &BucketRecord) -> String {
    revision_etag(None, bucket.metageneration)
}

/// The unquoted base64 protobuf message Cloud Storage uses as an ETag: the generation as
/// field 1 (objects only) and the metageneration as field 2 (field 1 for buckets). Recorded
/// responses carry the same value in the `ETag` header and the resource `etag` field. ACL
/// entries, buckets, objects and gRPC messages all share this encoding.
pub(crate) fn revision_etag(generation: Option<i64>, metageneration: i64) -> String {
    fn varint(bytes: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            let [byte, ..] = value.to_le_bytes();
            bytes.push(byte | 0x80);
            value >>= 7;
        }
        let [byte, ..] = value.to_le_bytes();
        bytes.push(byte);
    }
    let mut bytes = Vec::new();
    match generation {
        Some(generation) => {
            bytes.push(0x08);
            varint(&mut bytes, generation.cast_unsigned());
            bytes.push(0x10);
        }
        None => bytes.push(0x08),
    }
    varint(&mut bytes, metageneration.cast_unsigned());
    crate::base64::encode(&bytes)
}

/// The tag of an IAM policy: the base64 of its big-endian update sequence.
pub(crate) fn policy_etag(sequence: i64) -> String {
    crate::base64::encode(&sequence.to_be_bytes())
}
