//! Validated lifecycle metadata; execution belongs to the maintenance scheduler.

use super::{CatalogError, StorageClass, validate_storage_class};
use crate::metadata_error::{LifecycleExample, MetadataError};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, str::FromStr};

pub(crate) mod evaluate;
mod size;
pub(crate) use size::LifecycleSize;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LifecycleRule {
    pub action: LifecycleAction,
    pub condition: LifecycleCondition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "storageClass")]
pub(crate) enum LifecycleAction {
    Delete,
    SetStorageClass(StorageClass),
    AbortIncompleteMultipartUpload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub(crate) enum LifecycleMatchClass {
    Supported(StorageClass),
    Regional,
}

impl LifecycleMatchClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Supported(class) => class.as_str(),
            Self::Regional => "REGIONAL",
        }
    }
}

impl FromStr for LifecycleMatchClass {
    type Err = CatalogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "REGIONAL" => Ok(Self::Regional),
            value => value.parse().map(Self::Supported),
        }
    }
}

impl TryFrom<String> for LifecycleMatchClass {
    type Error = CatalogError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

crate::serde_util::serialize_as_str!(LifecycleMatchClass);

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LifecycleCondition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_before: Option<LifecycleDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_time_before: Option<LifecycleDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_since_custom_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_since_noncurrent_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches_prefix: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches_storage_class: Vec<LifecycleMatchClass>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches_suffix: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noncurrent_time_before: Option<LifecycleDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_newer_versions: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_above_bytes: Option<LifecycleSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_below_bytes: Option<LifecycleSize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleDate {
    pub year: i32,
    pub month: i32,
    pub day: i32,
}

impl serde::Serialize for LifecycleDate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!(
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        ))
    }
}

impl<'de> serde::Deserialize<'de> for LifecycleDate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        let mut parts = value.split('-');
        let mut component = || -> Result<i32, D::Error> {
            parts
                .next()
                .ok_or_else(|| serde::de::Error::custom("Invalid lifecycle date"))?
                .parse()
                .map_err(serde::de::Error::custom)
        };
        let date = Self {
            year: component()?,
            month: component()?,
            day: component()?,
        };
        if parts.next().is_some() {
            return Err(serde::de::Error::custom("Invalid lifecycle date"));
        }
        Ok(date)
    }
}

impl LifecycleRule {
    #[cfg(test)]
    pub(crate) fn to_stored(rules: &[Self]) -> Result<Vec<Value>, CatalogError> {
        let values = serialize_rules(rules).map_err(|error| invalid_rule(&error))?;
        validate(&values)?;
        Ok(values)
    }

    pub(crate) fn from_input(values: &[Value]) -> Result<Vec<Self>, CatalogError> {
        validate(values)?;
        deserialize_rules(values).map_err(|error| invalid_rule(&error))
    }

    pub(crate) fn from_stored(values: &[Value]) -> Result<Vec<Self>, CatalogError> {
        // Stored values must satisfy the same contract before conversions narrow integers.
        validate(values).map_err(|error| {
            crate::storage::failure::unavailable(CatalogError::Corrupt(
                crate::storage::failure::BackendCause::from_error(error),
            ))
        })?;
        deserialize_rules(values).map_err(|error| {
            crate::storage::failure::unavailable(CatalogError::Corrupt(
                crate::storage::failure::BackendCause::from_error(error),
            ))
        })
    }
}

fn deserialize_rules(values: &[Value]) -> Result<Vec<LifecycleRule>, serde_json::Error> {
    values.iter().map(LifecycleRule::deserialize).collect()
}

fn serialize_rules(rules: &[LifecycleRule]) -> Result<Vec<Value>, serde_json::Error> {
    rules.iter().map(serde_json::to_value).collect()
}

fn invalid_rule(error: &serde_json::Error) -> CatalogError {
    CatalogError::InvalidMetadata(format!("Invalid lifecycle rule: {error}"))
}

fn invalid(field: &str, example: impl Into<LifecycleExample>) -> CatalogError {
    CatalogError::Metadata(MetadataError::Lifecycle {
        field: field.into(),
        example: example.into(),
    })
}

fn validate_class(value: &str, field: &str) -> Result<(), CatalogError> {
    validate_storage_class(value)
        .map(|_| ())
        .map_err(|error| match error {
            CatalogError::Metadata(error) if error.unsupported() => {
                CatalogError::Metadata(MetadataError::LifecycleClass {
                    field: field.into(),
                    value: value.into(),
                })
            }
            CatalogError::InvalidMetadata(_) => {
                invalid(field, "a supported bucket class, for example COLDLINE")
            }
            error => error,
        })
}

pub(crate) fn parse(value: &Value) -> Result<Vec<Value>, CatalogError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let config = value
        .as_object()
        .ok_or_else(|| invalid("configuration", "{\"rule\":[]}"))?;
    let Some(rules) = config.get("rule").filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let rules = rules
        .as_array()
        .ok_or_else(|| invalid("rule", "an array, for example []"))?;
    let mut rules = rules.clone();
    // Ruby serializes unused optional conditions as null and lists as empty arrays.
    for rule in &mut rules {
        if let Some(action) = rule.get_mut("action").and_then(Value::as_object_mut)
            && action.get("storageClass").is_some_and(Value::is_null)
        {
            action.remove("storageClass");
        }
        if let Some(condition) = rule.get_mut("condition").and_then(Value::as_object_mut) {
            condition.retain(|field, value| {
                !CONDITIONS.contains(&field.as_str())
                    || !(value.is_null()
                        || (matches!(
                            field.as_str(),
                            "matchesPrefix" | "matchesSuffix" | "matchesStorageClass"
                        ) && value.as_array().is_some_and(Vec::is_empty)))
            });
        }
    }
    // The typed round trip ignores unknown action fields and canonicalizes int64 strings.
    serialize_rules(&LifecycleRule::from_input(&rules)?).map_err(|error| invalid_rule(&error))
}

const CONDITIONS: &[&str] = &[
    "age",
    "createdBefore",
    "customTimeBefore",
    "daysSinceCustomTime",
    "daysSinceNoncurrentTime",
    "isLive",
    "matchesStorageClass",
    "matchesPrefix",
    "matchesSuffix",
    "noncurrentTimeBefore",
    "numNewerVersions",
    "sizeAboveBytes",
    "sizeBelowBytes",
];

pub(crate) fn validate(rules: &[Value]) -> Result<(), CatalogError> {
    // `MAX_LIFECYCLE_RULES_PER_BUCKET` in the pinned `storage.proto`.
    if rules.len() > 100 {
        return Err(invalid("rule count (maximum 100)", "100 or fewer rules"));
    }
    let mut prefixes_suffixes = 0;
    for (rule_index, rule) in rules.iter().enumerate() {
        let action = rule
            .get("action")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("action", "{\"type\":\"Delete\"}"))?;
        let kind = action
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(
            kind,
            "Delete" | "SetStorageClass" | "AbortIncompleteMultipartUpload"
        ) {
            return Err(invalid(
                "action.type",
                "Delete, SetStorageClass or AbortIncompleteMultipartUpload",
            ));
        }
        if kind == "SetStorageClass" {
            let class = action
                .get("storageClass")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid(
                        "action.storageClass",
                        "a supported class, for example COLDLINE",
                    )
                })?;
            validate_class(class, &format!("rule[{rule_index}].action.storageClass"))?;
        } else if action.contains_key("storageClass") {
            return Err(invalid(
                "action.storageClass",
                LifecycleExample::StorageClassAction,
            ));
        }
        let condition = rule
            .get("condition")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("condition", LifecycleExample::AgeCondition))?;
        let mut count = 0;
        for (field, value) in condition {
            if field == "matchesPattern" {
                return Err(CatalogError::UnsupportedMetadata(format!(
                    "lifecycle.condition.{field}"
                )));
            }
            if !CONDITIONS.contains(&field.as_str()) {
                return Err(invalid(field, LifecycleExample::DocumentedCondition));
            }
            count += 1;
            if kind == "AbortIncompleteMultipartUpload"
                && !matches!(field.as_str(), "age" | "matchesPrefix" | "matchesSuffix")
            {
                return Err(invalid(
                    "AbortIncompleteMultipartUpload condition",
                    LifecycleExample::AbortCondition,
                ));
            }
            prefixes_suffixes += validate_condition(field, value, rule_index)?;
        }
        if count == 0 {
            return Err(invalid("empty condition", LifecycleExample::EmptyCondition));
        }
    }
    if prefixes_suffixes > 1000 {
        return Err(invalid(
            "prefix/suffix count (maximum 1000)",
            "at most 1000 prefixes and suffixes across all rules",
        ));
    }
    Ok(())
}

/// Validates one lifecycle condition value and returns how many prefixes or suffixes it adds.
fn validate_condition(
    field: &str,
    value: &Value,
    rule_index: usize,
) -> Result<usize, CatalogError> {
    match field {
        "age" | "daysSinceCustomTime" | "daysSinceNoncurrentTime" | "numNewerVersions" => {
            if !value
                .as_i64()
                .is_some_and(|number| (0..=i64::from(i32::MAX)).contains(&number))
            {
                return Err(invalid(field, "a nonnegative int32, for example 30"));
            }
        }
        "createdBefore" | "customTimeBefore" | "noncurrentTimeBefore" => {
            if !value.as_str().is_some_and(valid_date) {
                return Err(invalid(
                    field,
                    "a valid YYYY-MM-DD date, for example 2026-10-27",
                ));
            }
        }
        "isLive" => {
            if !value.is_boolean() {
                return Err(invalid(field, "true or false"));
            }
        }
        "sizeAboveBytes" | "sizeBelowBytes" => {
            LifecycleSize::deserialize(value).map_err(|_| {
                invalid(
                    field,
                    "an int64 from 0 to 5497558138880 (5 TiB), as a string or number",
                )
            })?;
        }
        _ => {
            let values = value.as_array().ok_or_else(|| {
                invalid(field, "a nonempty string array, for example [\"archive/\"]")
            })?;
            let mut unique = BTreeSet::new();
            if values.is_empty() {
                return Err(invalid(field, "a nonempty string array"));
            }
            for value in values {
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid(field, "string values"))?;
                if field == "matchesStorageClass" {
                    if value != "REGIONAL" {
                        validate_class(
                            value,
                            &format!("rule[{rule_index}].condition.matchesStorageClass"),
                        )?;
                    }
                } else if !unique.insert(value) {
                    return Err(invalid(
                        field,
                        "distinct prefixes or suffixes, for example [\"archive/\"]",
                    ));
                }
            }
            if field != "matchesStorageClass" {
                return Ok(values.len());
            }
        }
    }
    Ok(0)
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return false;
    }
    let number =
        |range: std::ops::Range<usize>| value.get(range).and_then(|part| part.parse::<u32>().ok());
    let (Some(year), Some(month), Some(day)) = (number(0..4), number(5..7), number(8..10)) else {
        return false;
    };
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            28 + u32::from(
                year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)),
            )
        }
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    year > 0 && day > 0 && day <= days
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleLocation {
    Region,
    MultiRegion,
    Unknown,
}

impl From<&str> for LifecycleLocation {
    fn from(value: &str) -> Self {
        match value {
            "region" => Self::Region,
            "dual-region" | "multi-region" => Self::MultiRegion,
            _ => Self::Unknown,
        }
    }
}

impl LifecycleLocation {
    pub(crate) fn permits(self, action: LifecycleAction) -> bool {
        match action {
            LifecycleAction::SetStorageClass(class) => match self {
                Self::Region => class != StorageClass::MultiRegional,
                Self::MultiRegion => true,
                Self::Unknown => false,
            },
            LifecycleAction::Delete | LifecycleAction::AbortIncompleteMultipartUpload => true,
        }
    }
}

pub(crate) fn validate_location(
    rules: &[LifecycleRule],
    location_type: &str,
) -> Result<(), CatalogError> {
    let incompatible = match LifecycleLocation::from(location_type) {
        LifecycleLocation::Region => LifecycleMatchClass::Supported(StorageClass::MultiRegional),
        LifecycleLocation::MultiRegion | LifecycleLocation::Unknown => {
            LifecycleMatchClass::Regional
        }
    };
    for rule in rules {
        if rule.condition.matches_storage_class.contains(&incompatible) {
            return Err(invalid(
                "matchesStorageClass for bucket location",
                "STANDARD instead of a legacy class from another location type",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_lifecycle_preserves_all_actions_and_legacy_class_conditions() {
        for action in [
            json!({"type":"Delete"}),
            json!({"type":"AbortIncompleteMultipartUpload"}),
            json!({"type":"SetStorageClass","storageClass":"NEARLINE"}),
        ] {
            let stored = vec![json!({"action":action,"condition":{"age":1}})];
            let typed = LifecycleRule::from_stored(&stored).expect("existing action");
            assert_eq!(
                serde_json::to_string(&LifecycleRule::to_stored(&typed).expect("stored action"))
                    .unwrap(),
                serde_json::to_string(&stored).unwrap()
            );
        }
        let stored = vec![
            json!({"action":{"type":"Delete"},"condition":{"matchesStorageClass":["REGIONAL","DURABLE_REDUCED_AVAILABILITY","MULTI_REGIONAL"]}}),
        ];
        let typed = LifecycleRule::from_stored(&stored).expect("legacy class matches");
        assert_eq!(
            serde_json::to_string(&LifecycleRule::to_stored(&typed).expect("stored class matches"))
                .unwrap(),
            serde_json::to_string(&stored).unwrap()
        );
    }

    #[test]
    fn typed_lifecycle_preserves_every_condition_and_rejects_invalid_values() {
        let values = vec![
            json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":30,"createdBefore":"2026-01-01","customTimeBefore":"2026-02-01","daysSinceCustomTime":2,"daysSinceNoncurrentTime":3,"isLive":false,"matchesPrefix":["a/"],"matchesStorageClass":["STANDARD"],"matchesSuffix":[".txt"],"noncurrentTimeBefore":"2026-03-01","numNewerVersions":4}}),
        ];
        let typed = LifecycleRule::from_stored(&values).unwrap();
        assert_eq!(
            serde_json::to_string(&LifecycleRule::to_stored(&typed).unwrap()).unwrap(),
            serde_json::to_string(&values).unwrap()
        );
        let mut invalid_age = typed.clone();
        invalid_age[0].condition.age = Some(-1);
        assert!(
            LifecycleRule::to_stored(&invalid_age)
                .unwrap_err()
                .to_string()
                .contains("nonnegative int32")
        );
        let mut invalid_date = typed;
        invalid_date[0].condition.created_before = Some(LifecycleDate {
            year: 2025,
            month: 2,
            day: 29,
        });
        assert!(
            LifecycleRule::to_stored(&invalid_date)
                .unwrap_err()
                .to_string()
                .contains("valid YYYY-MM-DD")
        );
        assert!(matches!(
            LifecycleRule::from_stored(&[json!({})]),
            Err(CatalogError::Corrupt(_))
        ));
    }

    #[test]
    fn lifecycle_reads_preserve_stored_key_order_and_new_writes_are_canonical() {
        let original = r#"[{"condition":{"isLive":true,"age":1},"action":{"type":"Delete"}}]"#;
        let stored: Vec<Value> = serde_json::from_str(original).unwrap();
        let typed = LifecycleRule::from_stored(&stored).unwrap();
        assert_eq!(serde_json::to_string(&stored).unwrap(), original);
        let canonical = LifecycleRule::to_stored(&typed).unwrap();
        assert_eq!(canonical, stored);
        let canonical_json =
            r#"[{"action":{"type":"Delete"},"condition":{"age":1,"isLive":true}}]"#;
        assert_eq!(serde_json::to_string(&canonical).unwrap(), canonical_json);
        let parsed = parse(&json!({"rule":stored})).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), canonical_json);
    }

    #[test]
    fn lifecycle_input_ignores_unknown_action_fields_and_classifies_invalid_rules() {
        for action in [
            json!({"type":"Delete"}),
            json!({"type":"SetStorageClass","storageClass":"COLDLINE"}),
            json!({"type":"AbortIncompleteMultipartUpload"}),
        ] {
            let mut input = action.clone();
            input["futureField"] = json!({"nested":[true, 1]});
            let parsed = parse(&json!({"rule":[{"action":input,"condition":{"age":1}}]})).unwrap();
            assert_eq!(parsed[0]["action"], action);
            assert_eq!(
                serde_json::to_string(&parsed[0]["action"]).unwrap(),
                serde_json::to_string(&action).unwrap()
            );
        }
        let invalid = [json!({"action":{"type":"unknown"},"condition":{"age":1}})];
        assert!(matches!(
            LifecycleRule::from_input(&invalid),
            Err(CatalogError::Metadata(_))
        ));
        assert!(matches!(
            parse(&json!({"rule":invalid})),
            Err(CatalogError::Metadata(_))
        ));
        assert!(matches!(
            LifecycleRule::from_stored(&invalid),
            Err(CatalogError::Corrupt(_))
        ));
    }

    #[test]
    fn lifecycle_date_and_prefix_suffix_boundaries() {
        for date in ["2000-02-29", "2024-02-29", "0001-01-01", "9999-12-31"] {
            assert!(valid_date(date), "{date}");
        }
        for date in [
            "1900-02-29",
            "2025-02-29",
            "0000-01-01",
            "2026-04-31",
            "2026-1-01",
            "2026-01-00",
            "日本語日付",
        ] {
            assert!(!valid_date(date), "{date}");
        }
        let prefixes: Vec<_> = (0..1000).map(|number| format!("prefix-{number}")).collect();
        let mut rules =
            vec![json!({"action":{"type":"Delete"},"condition":{"matchesPrefix":prefixes}})];
        validate(&rules).unwrap();
        rules[0]["condition"]["matchesSuffix"] = json!([".txt"]);
        assert!(validate(&rules).is_err());
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            validate(&[json!({"action":{"type":"SetStorageClass","storageClass":class},"condition":{"age":0}})]).unwrap();
        }
    }
}
