use super::{names, pb, status};
use crate::json;
use tonic::{Status, metadata::MetadataMap};

pub(super) fn check(
    metadata: &MetadataMap,
    resources: Option<&[(&str, &str)]>,
) -> Result<(), Status> {
    if metadata.contains_key("x-goog-user-project") {
        return Err(status::unsupported(
            "x-goog-user-project billing metadata; omit UserProject",
        ));
    }
    for metadata in metadata.get_all("x-goog-request-params").iter() {
        let value = metadata.to_str().map_err(|_| {
            Status::invalid_argument(
                "Invalid routing metadata; use bucket=projects%2F_%2Fbuckets%2Fexample-bucket",
            )
        })?;
        for parameter in value.split('&') {
            let (key, value) = parameter.split_once('=').ok_or_else(|| {
                Status::invalid_argument("Routing metadata must contain key=value pairs")
            })?;
            let key = json::decode(key, json::Decoding::Form)?;
            let value = json::decode(value, json::Decoding::Form)?;
            if key == "bucket" && names::bucket_name(&value).is_err() {
                return Err(status::invalid_with_reason(
                    format!(
                        "Invalid bucket name found in x-goog-request-params metadata (bucket={value}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
                    ),
                    "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT",
                ));
            }
            if key == "direct_connectivity_diagnostic" {
                continue;
            }
            let Some(resources) = resources else {
                // Stream resources arrive in their first protobuf message.
                if !matches!(key.as_str(), "bucket" | "appendable" | "source_bucket") {
                    return Err(status::unsupported("routing metadata field"));
                }
                continue;
            };
            let expected = resources
                .iter()
                .find(|(field, _)| *field == key)
                .ok_or_else(|| status::unsupported("routing metadata field"))?
                .1;
            let matches = if matches!(key.as_str(), "bucket" | "source_bucket") {
                names::bucket_name(&value)? == names::bucket_name(expected).unwrap_or(expected)
            } else {
                value == expected
            };
            if !matches {
                return Err(Status::invalid_argument(
                    "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn upload_bucket(upload_id: &str) -> &str {
    upload_id.split_once('/').map_or("", |(bucket, _)| bucket)
}

pub(super) fn iam_bucket(resource: &str) -> &str {
    resource
        .split_once("/managedFolders/")
        .or_else(|| resource.split_once("/objects/"))
        .map_or(resource, |(bucket, _)| bucket)
}

pub(super) fn write(
    metadata: &MetadataMap,
    first: Option<&pb::bidi_write_object_request::FirstMessage>,
) -> Result<(), Status> {
    use pb::bidi_write_object_request::FirstMessage;
    let (bucket, appendable) = match first {
        Some(FirstMessage::WriteObjectSpec(spec)) => (
            spec.resource.as_ref().map_or("", |r| r.bucket.as_str()),
            if spec.appendable == Some(true) {
                "true"
            } else {
                "false"
            },
        ),
        Some(FirstMessage::AppendObjectSpec(spec)) => (spec.bucket.as_str(), "true"),
        Some(FirstMessage::UploadId(id)) => (upload_bucket(id), "false"),
        None => ("", "false"),
    };
    check(
        metadata,
        Some(&[("bucket", bucket), ("appendable", appendable)]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_validates_all_values_and_resumed_upload_buckets() {
        let mut metadata = MetadataMap::new();
        metadata.insert(
            "x-goog-request-params",
            "bucket=projects%2F_%2Fbuckets%2Ftest-bucket&appendable=false"
                .parse()
                .unwrap(),
        );
        let first = pb::bidi_write_object_request::FirstMessage::UploadId(format!(
            "test-bucket/{}",
            "a".repeat(64)
        ));
        write(&metadata, Some(&first)).unwrap();
        let wrong = pb::bidi_write_object_request::FirstMessage::UploadId(format!(
            "other-bucket/{}",
            "a".repeat(64)
        ));
        assert_eq!(
            write(&metadata, Some(&wrong)).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            check(&metadata, Some(&[("project", "projects/p")]))
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented
        );
        metadata.insert("x-goog-request-params", "bucket=bad".parse().unwrap());
        let error = check(&metadata, None).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(!error.details().is_empty());
    }
}
