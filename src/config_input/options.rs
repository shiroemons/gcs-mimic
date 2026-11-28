#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::num::ParseIntError;

// Only parsed: the commands print URLs, never the style's name.
crate::choice::string_enum! {
    table
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum UrlStyle {
        Path => "path",
        VirtualHosted => "virtual-hosted",
    };
    from_str(crate::choice::InvalidChoice,
        _ => Err(crate::choice::InvalidChoice("style must be path or virtual-hosted")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("options require --name VALUE pairs")]
pub(crate) enum PairError {
    MissingValue,
}

pub(crate) fn option_pairs(
    args: &[String],
) -> impl Iterator<Item = Result<(&str, &str), PairError>> {
    args.chunks(2).map(|pair| match pair {
        [name, value] => Ok((name.as_str(), value.as_str())),
        _ => Err(PairError::MissingValue),
    })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DurationError {
    #[error("unsupported duration unit")]
    Unit,
    #[error("invalid duration number")]
    Number(#[source] ParseIntError),
    #[error("duration is too large")]
    Overflow,
}

pub(crate) fn duration(value: &str, units: &[(&str, u64)]) -> Result<u64, DurationError> {
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
pub(crate) struct Fix(pub String);

impl From<String> for Fix {
    fn from(fix: String) -> Self {
        Self(fix)
    }
}

impl From<crate::config::buckets_document::shorthand::ShorthandError> for Fix {
    fn from(error: crate::config::buckets_document::shorthand::ShorthandError) -> Self {
        Self(error.to_string())
    }
}

impl From<crate::config::InvalidChoice> for Fix {
    fn from(error: crate::config::InvalidChoice) -> Self {
        Self(error.to_string())
    }
}

impl From<crate::notification::publisher::InvalidEndpoint> for Fix {
    fn from(error: crate::notification::publisher::InvalidEndpoint) -> Self {
        Self(error.to_string())
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
    use std::assert_matches;

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
        assert_matches!(duration("1m", &units), Err(DurationError::Unit));
        assert_matches!(duration("s", &units), Err(DurationError::Number(_)));
        assert_matches!(
            duration("18446744073709551615s", &units),
            Err(DurationError::Overflow)
        );
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = PairError::MissingValue;
    assert_eq!(error.to_string(), "options require --name VALUE pairs");
    assert!(std::error::Error::source(&error).is_none());
    let error = DurationError::Number("x".parse::<u64>().unwrap_err());
    assert_eq!(error.to_string(), "invalid duration number");
    assert_eq!(
        crate::error::Report::chain(&error).to_string(),
        "invalid duration number: invalid digit found in string"
    );
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<ParseIntError>()
    );
}
