//! Concrete object and bucket generations.

use std::num::NonZeroI64;

/// A concrete object or bucket generation: always positive. Request parameters that may be 0 or
/// negative (`generation=0` answers 404, `ifGenerationMatch=0` means "absent") stay `i64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct Generation(NonZeroI64);

/// A generation that is zero or negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidGeneration;

impl std::fmt::Display for InvalidGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("generation must be positive")
    }
}

impl std::error::Error for InvalidGeneration {}

impl Generation {
    pub fn get(self) -> i64 {
        self.0.get()
    }
}

impl TryFrom<i64> for Generation {
    type Error = InvalidGeneration;

    fn try_from(value: i64) -> Result<Self, InvalidGeneration> {
        NonZeroI64::new(value)
            .filter(|value| value.get() > 0)
            .map(Self)
            .ok_or(InvalidGeneration)
    }
}

impl From<Generation> for i64 {
    fn from(generation: Generation) -> Self {
        generation.get()
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
