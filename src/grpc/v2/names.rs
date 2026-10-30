use tonic::Status;

pub(crate) fn bucket_name(resource: &str) -> Result<&str, Status> {
    resource
        .strip_prefix("projects/_/buckets/")
        .filter(|name| {
            !name.is_empty() && !name.contains('/') && !name.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            Status::invalid_argument(
                "Invalid bucket resource name; use projects/_/buckets/example-bucket",
            )
        })
}

pub(crate) fn object_name<'a>(
    bucket: &'a str,
    object: &'a str,
) -> Result<(&'a str, &'a str), Status> {
    let bucket = bucket_name(bucket)?;
    if object.is_empty() || object.len() > 1024 || object.chars().any(char::is_control) {
        return Err(Status::invalid_argument(
            "Invalid object name; use a nonempty name of at most 1024 UTF-8 bytes, for example folder/object.txt",
        ));
    }
    Ok((bucket, object))
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
        for name in ["", "a\nb", &"x".repeat(1025)] {
            assert_eq!(
                object_name("projects/_/buckets/bucket", name)
                    .unwrap_err()
                    .code(),
                tonic::Code::InvalidArgument
            );
        }
    }
}
