//! Checked integer representations used by the protobuf adapters.

use crate::{object::ObjectError, storage::failure::BackendCause};

pub(super) fn signed(value: u64, field: &str) -> Result<i64, ObjectError> {
    i64::try_from(value).map_err(|error| {
        ObjectError::Corrupt(BackendCause::with_source(
            format!("{field} value {value} is outside the protobuf int64 range"),
            error,
        ))
    })
}

pub(super) fn component_count(value: Option<i64>) -> Result<i32, ObjectError> {
    let value = value.unwrap_or_default();
    i32::try_from(value).map_err(|error| {
        ObjectError::Corrupt(BackendCause::with_source(
            format!("component_count value {value} is outside the protobuf int32 range"),
            error,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_wire_integers_reject_out_of_range_stored_values() {
        use std::error::Error;

        assert_eq!(signed(0, "size").unwrap(), 0);
        assert_eq!(signed(i64::MAX.cast_unsigned(), "size").unwrap(), i64::MAX);
        assert_eq!(component_count(None).unwrap(), 0);
        assert_eq!(
            component_count(Some(i64::from(i32::MAX))).unwrap(),
            i32::MAX
        );
        assert_eq!(
            component_count(Some(i64::from(i32::MIN))).unwrap(),
            i32::MIN
        );
        for error in [
            signed(i64::MAX.cast_unsigned() + 1, "size").unwrap_err(),
            signed(u64::MAX, "size").unwrap_err(),
            component_count(Some(i64::from(i32::MAX) + 1)).unwrap_err(),
            component_count(Some(i64::from(i32::MIN) - 1)).unwrap_err(),
            component_count(Some(i64::MAX)).unwrap_err(),
            component_count(Some(i64::MIN)).unwrap_err(),
        ] {
            assert!(matches!(&error, ObjectError::Corrupt(_)));
            assert!(error.source().unwrap().is::<std::num::TryFromIntError>());
            assert!(error.to_string().contains(" value "));
            let status = tonic::Status::from(error);
            assert_eq!(status.code(), tonic::Code::Unavailable);
            assert_eq!(
                status.message(),
                "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
            );
        }
    }
}
