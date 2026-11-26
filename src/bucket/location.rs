//! Supported bucket locations, pinned to the official locations page checked in
//! sdk-regional-locations.md.

use super::CatalogError;
use std::str::FromStr;

const REGIONS: &[&str] = &[
    "AFRICA-SOUTH1",
    "ASIA-EAST1",
    "ASIA-EAST2",
    "ASIA-NORTHEAST1",
    "ASIA-NORTHEAST2",
    "ASIA-NORTHEAST3",
    "ASIA-SOUTH1",
    "ASIA-SOUTH2",
    "ASIA-SOUTHEAST1",
    "ASIA-SOUTHEAST2",
    "ASIA-SOUTHEAST3",
    "AUSTRALIA-SOUTHEAST1",
    "AUSTRALIA-SOUTHEAST2",
    "EUROPE-CENTRAL2",
    "EUROPE-NORTH1",
    "EUROPE-NORTH2",
    "EUROPE-SOUTHWEST1",
    "EUROPE-WEST1",
    "EUROPE-WEST2",
    "EUROPE-WEST3",
    "EUROPE-WEST4",
    "EUROPE-WEST6",
    "EUROPE-WEST8",
    "EUROPE-WEST9",
    "EUROPE-WEST10",
    "EUROPE-WEST12",
    "ME-CENTRAL1",
    "ME-CENTRAL2",
    "ME-WEST1",
    "NORTHAMERICA-NORTHEAST1",
    "NORTHAMERICA-NORTHEAST2",
    "NORTHAMERICA-SOUTH1",
    "SOUTHAMERICA-EAST1",
    "SOUTHAMERICA-WEST1",
    "US-CENTRAL1",
    "US-EAST1",
    "US-EAST4",
    "US-EAST5",
    "US-SOUTH1",
    "US-WEST1",
    "US-WEST2",
    "US-WEST3",
    "US-WEST4",
];
const DUAL_REGIONS: &[&str] = &["ASIA1", "EUR4", "EUR5", "EUR7", "EUR8", "NAM4"];
const MULTI_REGIONS: &[&str] = &["US", "EU", "ASIA"];

crate::choice::string_enum! {
    table
    /// A location type, as JSON `locationType` and Storage v2 `location_type` report it.
    /// The catalog stores these exact spellings, so any other stored text is corruption.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum LocationKind as pub(crate) as_str {
        Region => "region",
        DualRegion => "dual-region",
        MultiRegion => "multi-region",
    };
    listed(pub(self));
    sql(pub(crate) from_stored, "invalid stored location type")
}

impl LocationKind {
    pub(crate) fn names(self) -> &'static [&'static str] {
        match self {
            Self::Region => REGIONS,
            Self::DualRegion => DUAL_REGIONS,
            Self::MultiRegion => MULTI_REGIONS,
        }
    }
}

/// A supported location: a canonical upper-case name from the official table, which fixes its
/// type. Requests and configuration parse case-insensitively with `FromStr`; the catalog stores
/// and reads only the canonical name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    name: &'static str,
    kind: LocationKind,
}

impl Location {
    pub(super) const US: Self = Self {
        name: "US",
        kind: LocationKind::MultiRegion,
    };

    pub(crate) fn as_str(self) -> &'static str {
        self.name
    }

    pub(crate) fn kind(self) -> LocationKind {
        self.kind
    }

    /// The location whose canonical name is exactly `name`, as a catalog row stores it.
    pub(crate) fn from_canonical(name: &str) -> Option<Self> {
        Self::find(|candidate| candidate == name)
    }

    fn find(matches: impl Fn(&str) -> bool) -> Option<Self> {
        LocationKind::ALL.iter().copied().find_map(|kind| {
            kind.names()
                .iter()
                .copied()
                .find(|name| matches(name))
                .map(|name| Self { name, kind })
        })
    }
}

impl FromStr for Location {
    type Err = CatalogError;

    fn from_str(value: &str) -> Result<Self, CatalogError> {
        Self::find(|name| name.eq_ignore_ascii_case(value)).ok_or_else(|| {
            CatalogError::InvalidMetadata("The specified location constraint is not valid.".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_bucket_location_names_and_types() {
        for (names, kind) in [
            (REGIONS, LocationKind::Region),
            (DUAL_REGIONS, LocationKind::DualRegion),
            (MULTI_REGIONS, LocationKind::MultiRegion),
        ] {
            assert_eq!(LocationKind::from_stored(kind.as_str()), Some(kind));
            for name in names {
                let location: Location = name.to_ascii_lowercase().parse().unwrap();
                assert_eq!((location.as_str(), location.kind()), (*name, kind));
                assert_eq!(Location::from_canonical(name), Some(location));
                // The catalog stores only the canonical spelling.
                assert_eq!(Location::from_canonical(&name.to_ascii_lowercase()), None);
            }
        }
        for invalid in ["MARS-CENTRAL1", "US-CENTRAL1-A", "AU", "", " US"] {
            assert_eq!(
                invalid.parse::<Location>(),
                Err(CatalogError::InvalidMetadata(
                    "The specified location constraint is not valid.".into()
                ))
            );
            assert_eq!(Location::from_canonical(invalid), None);
        }
        for invalid in ["Region", "unknown", "", "multi_region"] {
            assert_eq!(LocationKind::from_stored(invalid), None);
        }
    }
}
