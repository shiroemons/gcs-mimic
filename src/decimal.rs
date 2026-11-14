//! Decimal integer text. Cloud Storage, the XML API and request signing each accept their own
//! spelling of a number, and recorded cases and tests pin the differences, so a parser names
//! its grammar and keeps its own error instead of sharing one rule.

use std::str::FromStr;

/// The text an integer accepts before its range is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grammar {
    /// `FromStr` of the target type: an optional `+` or `-`, then decimal digits.
    Signed,
    /// [`Self::Signed`] without a leading `+`: Cloud Storage refused `%2B1` for these
    /// parameters, while its `Invalid long value` answer covers any other malformed text.
    SignedNoPlus,
    /// One or more ASCII decimal digits and nothing else.
    Digits,
}

impl Grammar {
    /// Whether `text` is spelled in this grammar; the range of the target type is not checked.
    pub(crate) fn admits(self, text: &str) -> bool {
        match self {
            Self::Signed => true,
            Self::SignedNoPlus => !text.starts_with('+'),
            Self::Digits => !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
        }
    }

    /// The value of `text`, or `None` when the grammar refuses it or it is outside `T`'s range.
    pub(crate) fn parse<T: FromStr>(self, text: &str) -> Option<T> {
        self.admits(text).then(|| text.parse().ok()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::Grammar;

    #[test]
    fn each_grammar_accepts_its_own_spelling() {
        // (text, Signed, SignedNoPlus, Digits) as i64.
        let cases = [
            ("", None, None, None),
            ("0", Some(0), Some(0), Some(0)),
            ("007", Some(7), Some(7), Some(7)),
            ("+1", Some(1), None, None),
            ("-1", Some(-1), Some(-1), None),
            ("-0", Some(0), Some(0), None),
            (" 1", None, None, None),
            ("1 ", None, None, None),
            ("1.0", None, None, None),
            (
                "9223372036854775807",
                Some(i64::MAX),
                Some(i64::MAX),
                Some(i64::MAX),
            ),
            ("9223372036854775808", None, None, None),
            ("-9223372036854775808", Some(i64::MIN), Some(i64::MIN), None),
            ("-9223372036854775809", None, None, None),
        ];
        for (text, signed, no_plus, digits) in cases {
            assert_eq!(Grammar::Signed.parse::<i64>(text), signed, "{text:?}");
            assert_eq!(
                Grammar::SignedNoPlus.parse::<i64>(text),
                no_plus,
                "{text:?}"
            );
            assert_eq!(Grammar::Digits.parse::<i64>(text), digits, "{text:?}");
        }
    }

    #[test]
    fn grammars_bound_unsigned_and_narrow_types() {
        assert_eq!(Grammar::Digits.parse::<u32>("4294967295"), Some(u32::MAX));
        assert_eq!(Grammar::Digits.parse::<u32>("4294967296"), None);
        assert_eq!(Grammar::Digits.parse::<u32>("+4294967295"), None);
        assert_eq!(Grammar::Signed.parse::<u32>("+4294967295"), Some(u32::MAX));
        assert_eq!(Grammar::Signed.parse::<u32>("-1"), None);
        assert_eq!(Grammar::Signed.parse::<i32>("-1"), Some(-1));
        assert_eq!(Grammar::Signed.parse::<i32>("2147483648"), None);
        assert_eq!(Grammar::Digits.parse::<u16>("65535"), Some(u16::MAX));
        assert_eq!(Grammar::Digits.parse::<u16>("65536"), None);
        assert_eq!(
            Grammar::Digits.parse::<u64>("18446744073709551615"),
            Some(u64::MAX)
        );
        assert_eq!(Grammar::Digits.parse::<u64>("18446744073709551616"), None);
        // `admits` checks only the spelling, so an out-of-range number is admitted.
        assert!(Grammar::Digits.admits("18446744073709551616"));
        assert!(!Grammar::Digits.admits("1e3"));
        assert!(Grammar::SignedNoPlus.admits("abc"));
        assert!(!Grammar::SignedNoPlus.admits("+abc"));
    }
}
