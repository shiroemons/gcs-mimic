//! The component count of a copy or rewrite destination. Rewrite preserves composite identity in
//! the same location and storage class and collapses the count to one when either changes; the
//! adapters and the rewrite session retain the source snapshot for this decision.

use super::ObjectRecord;
use crate::bucket::StorageClass;

/// Whether a copy's destination bucket shares the source bucket's location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DestinationLocation {
    Same,
    Different,
}

impl DestinationLocation {
    pub(crate) fn of(source: Option<&str>, destination: &str) -> Self {
        if source == Some(destination) {
            Self::Same
        } else {
            Self::Different
        }
    }
}

pub(crate) fn derived_component_count(
    source: &ObjectRecord,
    destination_class: StorageClass,
    location: DestinationLocation,
) -> Option<i64> {
    source.component_count.map(|count| {
        if location == DestinationLocation::Same && destination_class == source.storage_class {
            count
        } else {
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_composite_keeps_its_count_only_in_the_same_location_and_class() {
        let mut source = crate::json::object_tests::record();
        assert_eq!(
            derived_component_count(&source, source.storage_class, DestinationLocation::Same),
            None
        );
        source.component_count = Some(3);
        assert_eq!(
            derived_component_count(&source, source.storage_class, DestinationLocation::Same),
            Some(3)
        );
        assert_eq!(
            derived_component_count(&source, StorageClass::Nearline, DestinationLocation::Same),
            Some(1)
        );
        assert_eq!(
            derived_component_count(
                &source,
                source.storage_class,
                DestinationLocation::Different
            ),
            Some(1)
        );
        assert_eq!(
            DestinationLocation::of(Some("US"), "US"),
            DestinationLocation::Same
        );
        assert_eq!(
            DestinationLocation::of(Some("US"), "EU"),
            DestinationLocation::Different
        );
        assert_eq!(
            DestinationLocation::of(None, "US"),
            DestinationLocation::Different
        );
    }
}
