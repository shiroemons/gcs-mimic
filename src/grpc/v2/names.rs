use tonic::Status;

pub(crate) fn bucket_name(resource: &str) -> Result<&str, Status> {
    resource
        .strip_prefix("projects/_/buckets/")
        .filter(|name| {
            !name.is_empty() && !name.contains('/')
        })
        .ok_or_else(|| {
            // Recorded on object RPCs; bucket RPCs reuse it because the recording cannot tell
            // whether Cloud Storage words them differently.
            super::status::invalid_with_reason(
                format!(
                    "Invalid bucket name '{resource}'. Bucket names must take the form 'projects/{{project-id}}/buckets/{{bucket-id}}'. The project-id must be '_'."
                ),
                "GRPC_INVALID_BUCKET",
            )
        })
}

#[cfg(test)]
pub(crate) fn object_name<'a>(
    bucket: &'a str,
    object: &'a str,
) -> Result<(&'a str, &'a str), Status> {
    let bucket = bucket_name(bucket)?;
    self::object(object)?;
    Ok((bucket, object))
}

pub(crate) fn object(name: &str) -> Result<(), Status> {
    use crate::object::name::InvalidObjectName;
    crate::object::name::validate(name).map_err(|error| match error {
        InvalidObjectName::Empty => super::status::invalid_field("No object name"),
        _ => Status::invalid_argument(
            "Invalid object name; use a nonempty name of at most 1024 UTF-8 bytes without CR/LF, '.', '..' or the .well-known/acme-challenge/ prefix, for example folder/object.txt",
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grpc_resource_name_parsing() {
        assert_eq!(
            bucket_name("projects/_/buckets/example-bucket").unwrap(),
            "example-bucket"
        );
        assert_eq!(
            object_name("projects/_/buckets/example-bucket", "a/b%2Fc").unwrap(),
            ("example-bucket", "a/b%2Fc")
        );
        for name in [
            "",
            "example-bucket",
            "projects/p/buckets/x",
            "projects/_/buckets/",
            "projects/_/buckets/a/b",
        ] {
            assert_eq!(
                bucket_name(name).unwrap_err().code(),
                tonic::Code::InvalidArgument
            );
        }
        for name in [
            "",
            ".",
            "..",
            ".well-known/acme-challenge/token",
            "a\nb",
            &"x".repeat(1025),
        ] {
            assert_eq!(
                object_name("projects/_/buckets/bucket", name)
                    .unwrap_err()
                    .code(),
                tonic::Code::InvalidArgument
            );
        }
        assert!(object_name("projects/_/buckets/bucket", "a\tb").is_ok());
    }
}
