//! Project numbers. Cloud Storage renders a bucket's project by number (`projectNumber`,
//! `projects/N`, `project-owners-N`, `service-N@...`), while requests name a project by ID. The
//! emulator keeps no project registry, so a project's number is a pure function of its ID: the
//! same ID gets the same number across restarts, data directories and machines.

use std::fmt;

/// A project's number. Real project numbers are positive and usually twelve digits long; a
/// derived number always has that shape, so a client that parses or validates it sees a
/// plausible value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProjectNumber(u64);

impl ProjectNumber {
    /// Project numbers are positive, so zero names no project.
    pub(crate) const ZERO: Self = Self(0);

    /// The smallest derived number and the count of derived values: every derived number has
    /// twelve digits. floci-gcp uses the same range and digest, so a project ID used against
    /// both emulators renders the same number.
    const DERIVED_BASE: u64 = 100_000_000_000;
    const DERIVED_SPAN: u64 = 900_000_000_000;

    /// The number of `project`. A project ID starts with a letter, so decimal digits already
    /// spell a number; any other text is an ID whose number is derived.
    pub(crate) fn of(project: &str) -> Self {
        Self::literal(project).unwrap_or_else(|| Self::derived(project))
    }

    /// The number `text` spells in decimal digits, leading zeros included, when it fits in 64
    /// bits. Digits beyond that range spell no number and are treated as an ID.
    pub(crate) fn literal(text: &str) -> Option<Self> {
        crate::decimal::Grammar::Digits.parse(text).map(Self)
    }

    /// `DERIVED_BASE + (SHA-256(id) mod DERIVED_SPAN)`, reading the digest as one big-endian
    /// unsigned integer.
    fn derived(id: &str) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, id.as_bytes());
        // The running remainder stays below 2^40, so shifting in a byte cannot overflow.
        let remainder = digest.as_ref().iter().fold(0, |remainder, &byte| {
            (remainder * 256 + u64::from(byte)) % Self::DERIVED_SPAN
        });
        Self(Self::DERIVED_BASE + remainder)
    }
}

impl fmt::Display for ProjectNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectNumber;

    #[test]
    fn an_id_derives_a_stable_twelve_digit_number() {
        // Computed independently with `shasum -a 256` and `bc`.
        for (id, number) in [
            ("local", "902648055030"),
            ("test-project", "900286375562"),
            ("my-project-123", "260684426529"),
            ("a", "179556493499"),
        ] {
            assert_eq!(ProjectNumber::of(id).to_string(), number, "{id}");
        }
    }

    #[test]
    fn decimal_digits_are_their_own_number() {
        for (digits, number) in [
            ("123", "123"),
            ("00123", "123"),
            ("0", "0"),
            ("000", "0"),
            ("18446744073709551615", "18446744073709551615"),
        ] {
            assert_eq!(ProjectNumber::of(digits).to_string(), number, "{digits}");
        }
        assert_eq!(ProjectNumber::literal("00"), Some(ProjectNumber::ZERO));
        for id in ["+123", "18446744073709551616", "1e3", ""] {
            assert_eq!(ProjectNumber::literal(id), None, "{id}");
            let derived = ProjectNumber::of(id).to_string();
            assert_eq!(derived.len(), 12, "{id}");
        }
    }
}
