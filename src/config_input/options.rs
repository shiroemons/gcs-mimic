#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::{fmt, num::ParseIntError};

crate::config::string_enum! {
    no_default pub enum UrlStyle {
        Path => "path",
        VirtualHosted => "virtual-hosted",
    }
    "style must be path or virtual-hosted"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairError {
    MissingValue,
}

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("options require --name VALUE pairs")
    }
}

impl std::error::Error for PairError {}

pub fn option_pairs(args: &[String]) -> impl Iterator<Item = Result<(&str, &str), PairError>> {
    args.chunks(2).map(|pair| match pair {
        [name, value] => Ok((name.as_str(), value.as_str())),
        _ => Err(PairError::MissingValue),
    })
}

#[derive(Debug)]
pub enum DurationError {
    Unit,
    Number(ParseIntError),
    Overflow,
}

impl fmt::Display for DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unit => f.write_str("unsupported duration unit"),
            Self::Number(error) => error.fmt(f),
            Self::Overflow => f.write_str("duration is too large"),
        }
    }
}

impl std::error::Error for DurationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Number(error) => Some(error),
            _ => None,
        }
    }
}

pub fn duration(value: &str, units: &[(&str, u64)]) -> Result<u64, DurationError> {
    let (number, factor) = units
        .iter()
        .find_map(|(suffix, factor)| value.strip_suffix(suffix).map(|number| (number, *factor)))
        .ok_or(DurationError::Unit)?;
    number
        .parse::<u64>()
        .map_err(DurationError::Number)?
        .checked_mul(factor)
        .ok_or(DurationError::Overflow)
}

/// User-facing repair text, copied verbatim into a diagnostic by its caller.
#[derive(Debug)]
pub struct Fix(pub String);

impl From<String> for Fix {
    fn from(fix: String) -> Self {
        Self(fix)
    }
}

impl From<&str> for Fix {
    fn from(fix: &str) -> Self {
        Self(fix.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_preserve_values_and_report_a_trailing_option_in_order() {
        let args = ["--name", "--literal", "--empty", "", "--missing"].map(str::to_owned);
        assert_eq!(
            option_pairs(&args).collect::<Vec<_>>(),
            [
                Ok(("--name", "--literal")),
                Ok(("--empty", "")),
                Err(PairError::MissingValue)
            ]
        );
    }

    #[test]
    fn durations_keep_units_overflow_and_numeric_errors_separate() {
        let units = [("ms", 1), ("s", 1000)];
        assert_eq!(duration("+1s", &units).ok(), Some(1000));
        assert_eq!(duration("0ms", &units).ok(), Some(0));
        assert!(matches!(duration("1m", &units), Err(DurationError::Unit)));
        assert!(matches!(
            duration("s", &units),
            Err(DurationError::Number(_))
        ));
        assert!(matches!(
            duration("18446744073709551615s", &units),
            Err(DurationError::Overflow)
        ));
    }
}
