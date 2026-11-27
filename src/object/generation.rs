//! Concrete object and bucket generations.

use std::num::{NonZeroI64, NonZeroU64};

macro_rules! positive_revision {
    ($(#[$doc:meta])* $visibility:vis struct $revision:ident, $invalid:ident, $message:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "i64", into = "i64")]
        $visibility struct $revision(NonZeroI64);

        #[doc = $message]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
        #[error($message)]
        $visibility struct $invalid;

        impl $revision {
            $visibility fn get(self) -> i64 {
                self.0.get()
            }

            #[cfg(test)]
            pub(crate) fn for_test(value: i64) -> Self {
                Self::try_from(value).unwrap()
            }
        }

        impl TryFrom<i64> for $revision {
            type Error = $invalid;

            fn try_from(value: i64) -> Result<Self, Self::Error> {
                NonZeroI64::new(value)
                    .filter(|value| value.get() > 0)
                    .map(Self)
                    .ok_or($invalid)
            }
        }

        impl From<$revision> for i64 {
            fn from(revision: $revision) -> Self {
                revision.get()
            }
        }

        impl std::fmt::Display for $revision {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

positive_revision! {
    /// A concrete object or bucket generation: always positive. Request parameters that may be 0
    /// or negative (`generation=0` answers 404, `ifGenerationMatch=0` means "absent") stay `i64`.
    pub struct Generation, InvalidGeneration, "generation must be positive"
}

positive_revision! {
    /// A positive metadata revision of an object or bucket.
    pub struct Metageneration, InvalidMetageneration, "metageneration must be positive"
}

impl Metageneration {
    // The smallest nonzero unsigned value is 1, which can be cast to signed in a constant.
    pub(crate) const INITIAL: Self = Self(NonZeroU64::MIN.cast_signed());

    pub(crate) fn checked_next(self) -> Option<Self> {
        let next = self.get().checked_add(1)?;
        NonZeroI64::new(next).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::{Generation, Metageneration};

    #[test]
    fn concrete_revisions_round_trip_as_signed_json_integers() {
        for (text, value) in [("1", 1), ("9223372036854775807", i64::MAX)] {
            let generation: Generation = serde_json::from_str(text).unwrap();
            let metageneration: Metageneration = serde_json::from_str(text).unwrap();
            assert_eq!(generation.get(), value);
            assert_eq!(metageneration.get(), value);
            assert_eq!(serde_json::to_string(&generation).unwrap(), text);
            assert_eq!(serde_json::to_string(&metageneration).unwrap(), text);
        }
        assert_eq!(Metageneration::INITIAL.checked_next().unwrap().get(), 2);
        assert!(
            Metageneration::try_from(i64::MAX)
                .unwrap()
                .checked_next()
                .is_none()
        );
        for value in [i64::MIN, -1, 0] {
            assert!(Generation::try_from(value).is_err());
            assert!(Metageneration::try_from(value).is_err());
        }
        for text in [
            "-9223372036854775808",
            "-1",
            "0",
            "9223372036854775808",
            "\"1\"",
        ] {
            assert!(serde_json::from_str::<Generation>(text).is_err());
            assert!(serde_json::from_str::<Metageneration>(text).is_err());
        }
    }
}
