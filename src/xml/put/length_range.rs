//! `x-goog-content-length-range` on XML object uploads.
//!
//! Cloud Storage documents the header as an inclusive `MIN,MAX` pair of byte counts for `PUT`
//! requests and answers a body outside it with `400 Bad Request`
//! (<https://docs.cloud.google.com/storage/docs/xml-api/reference-headers>). The reference names
//! no error codes or value grammar; the codes, messages, details and parse order here follow
//! responses recorded from Cloud Storage on 2026-11-04 (the `xml-content-length-range`
//! differential case). Rails Active Storage 8.1.4.1 signs this header into every GCS
//! direct-upload URL so the signature limits the uploaded size.

use super::{Error, XmlCode, single};
use hyper::HeaderMap;
use std::fmt;

pub(crate) const HEADER: &str = "x-goog-content-length-range";

/// The inclusive body sizes a PUT allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LengthRange {
    min: u64,
    max: u64,
}

/// Why a header value is not a range, in the order Cloud Storage checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    /// Splitting on commas gives other than two parts.
    NotAPair,
    MissingBound,
    /// A bound is not a signed 64-bit decimal. Cloud Storage accepts the forms
    /// `i64::from_str` accepts, including `+1`, `-0` and leading zeros, and rejects whitespace.
    Unparsable,
    Negative,
    Reversed,
}

/// A rejected header value with the reason Cloud Storage reports for it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MalformedRange<'a> {
    value: &'a str,
    reason: Reason,
}

/// Formats the recorded `Details` text.
impl fmt::Display for MalformedRange<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            Reason::NotAPair => write!(f, "The {HEADER} header you specified was invalid"),
            Reason::MissingBound => f.write_str("min and max range values cannot be null"),
            Reason::Negative => f.write_str("range values cannot be negative"),
            Reason::Unparsable | Reason::Reversed => {
                write!(f, "{HEADER}: '{}' is invalid", self.value)
            }
        }
    }
}

impl std::error::Error for MalformedRange<'_> {}

impl From<MalformedRange<'_>> for Error {
    fn from(error: MalformedRange<'_>) -> Self {
        let message = match error.reason {
            Reason::NotAPair => "Invalid argument.".to_owned(),
            _ => format!("The {HEADER} header you specified was invalid"),
        };
        let mut failure = Self::new(XmlCode::ContentLengthRangeInHeaderIsInvalid, message);
        failure.details = Some(error.to_string());
        failure
    }
}

impl<'a> TryFrom<&'a str> for LengthRange {
    type Error = MalformedRange<'a>;

    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        let malformed = |reason| MalformedRange { value, reason };
        let mut parts = value.split(',');
        let (Some(min), Some(max), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(malformed(Reason::NotAPair));
        };
        if min.is_empty() || max.is_empty() {
            return Err(malformed(Reason::MissingBound));
        }
        let (Ok(min), Ok(max)) = (min.parse::<i64>(), max.parse::<i64>()) else {
            return Err(malformed(Reason::Unparsable));
        };
        let (Ok(min), Ok(max)) = (u64::try_from(min), u64::try_from(max)) else {
            return Err(malformed(Reason::Negative));
        };
        if min > max {
            return Err(malformed(Reason::Reversed));
        }
        Ok(Self { min, max })
    }
}

impl LengthRange {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, Error> {
        let Some(value) = single(headers, HEADER)? else {
            return Ok(None);
        };
        Ok(Some(Self::try_from(value)?))
    }

    /// The largest body this range allows within `limit`, so a reader can stop at the first
    /// byte beyond it instead of reading the whole body.
    pub(crate) fn cap(self, limit: u64) -> u64 {
        self.max.min(limit)
    }

    /// The failure for a body larger than the range's maximum.
    pub(crate) fn exceeded() -> Error {
        let mut failure = Error::new(
            XmlCode::EntityTooLarge,
            "Your proposed upload is larger than the maximum object size specified in your Policy Document.",
        );
        failure.details = Some("Content-length exceeds upper bound on range".to_owned());
        failure
    }

    fn undersized() -> Error {
        let mut failure = Error::new(
            XmlCode::EntityTooSmall,
            "Your proposed upload is smaller than the minimum object size specified in your Policy Document.",
        );
        failure.details = Some("Content-length smaller than lower bound on range".to_owned());
        failure
    }

    pub(crate) fn check(self, size: u64) -> Result<(), Error> {
        if size > self.max {
            Err(Self::exceeded())
        } else if size < self.min {
            Err(Self::undersized())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Result<Option<LengthRange>, Error> {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER, value.parse().unwrap());
        LengthRange::from_headers(&headers)
    }

    #[test]
    fn absent_header_sets_no_range() {
        assert_eq!(LengthRange::from_headers(&HeaderMap::new()).unwrap(), None);
    }

    #[test]
    fn accepts_the_signed_decimal_forms_cloud_storage_accepted() {
        for (value, min, max) in [
            ("0,256", 0, 256),
            ("15,15", 15, 15),
            ("+1,5", 1, 5),
            ("-0,5", 0, 5),
            ("01,05", 1, 5),
            ("0,9223372036854775807", 0, 9_223_372_036_854_775_807),
        ] {
            assert_eq!(
                parse(value).unwrap(),
                Some(LengthRange { min, max }),
                "{value}"
            );
        }
    }

    /// Each value, its recorded `Message` and `Details`.
    #[test]
    fn malformed_values_report_the_recorded_reason() {
        let invalid = "The x-goog-content-length-range header you specified was invalid";
        for (value, message, details) in [
            ("5", "Invalid argument.", invalid.to_owned()),
            ("1,2,3", "Invalid argument.", invalid.to_owned()),
            (
                "1,",
                invalid,
                "min and max range values cannot be null".to_owned(),
            ),
            (
                ",",
                invalid,
                "min and max range values cannot be null".to_owned(),
            ),
            (
                "-1,5",
                invalid,
                "range values cannot be negative".to_owned(),
            ),
            (
                "5,-1",
                invalid,
                "range values cannot be negative".to_owned(),
            ),
            ("6,5", invalid, format!("{HEADER}: '6,5' is invalid")),
            ("a,b", invalid, format!("{HEADER}: 'a,b' is invalid")),
            ("1, 10", invalid, format!("{HEADER}: '1, 10' is invalid")),
            (
                "0,9223372036854775808",
                invalid,
                format!("{HEADER}: '0,9223372036854775808' is invalid"),
            ),
        ] {
            let error = parse(value).unwrap_err();
            assert_eq!(
                error.code,
                XmlCode::ContentLengthRangeInHeaderIsInvalid,
                "{value:?}"
            );
            assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST, "{value:?}");
            assert_eq!(error.message, message, "{value:?}");
            assert_eq!(
                error.details.as_deref(),
                Some(details.as_str()),
                "{value:?}"
            );
        }
    }

    #[test]
    fn repeated_header_fails_like_other_single_value_headers() {
        let mut headers = HeaderMap::new();
        headers.append(HEADER, "1,2".parse().unwrap());
        headers.append(HEADER, "1,2".parse().unwrap());
        assert_eq!(
            LengthRange::from_headers(&headers).unwrap_err().code,
            XmlCode::ExcessHeaderValues
        );
    }

    #[test]
    fn bounds_are_inclusive_and_name_the_failed_side() {
        let range = LengthRange { min: 3, max: 5 };
        for size in [3, 4, 5] {
            assert!(range.check(size).is_ok(), "{size}");
        }
        let small = range.check(2).unwrap_err();
        assert_eq!(small.code, XmlCode::EntityTooSmall);
        assert_eq!(
            small.details.as_deref(),
            Some("Content-length smaller than lower bound on range")
        );
        let large = range.check(6).unwrap_err();
        assert_eq!(large.code, XmlCode::EntityTooLarge);
        assert_eq!(
            large.details.as_deref(),
            Some("Content-length exceeds upper bound on range")
        );
        for error in [small, large] {
            assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn cap_never_exceeds_the_configured_limit() {
        let range = LengthRange { min: 0, max: 10 };
        assert_eq!(range.cap(4), 4);
        assert_eq!(range.cap(100), 10);
    }
}
