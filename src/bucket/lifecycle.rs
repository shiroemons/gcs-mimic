//! Validated lifecycle metadata; execution belongs to the maintenance scheduler.

use super::{
    CatalogError, ClassLocationConflict, InvalidStorageClass, Location, LocationKind, StorageClass,
};
use crate::cause::BackendCause;
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LifecycleCondition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<ConditionInt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_before: Option<LifecycleDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_time_before: Option<LifecycleDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_since_custom_time: Option<ConditionInt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_since_noncurrent_time: Option<ConditionInt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches_prefix: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches_storage_class: Vec<StorageClass>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches_suffix: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noncurrent_time_before: Option<LifecycleDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_newer_versions: Option<ConditionInt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_above_bytes: Option<LifecycleSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_below_bytes: Option<LifecycleSize>,
}

/// A condition's day or version count: a nonnegative int32, the range the lifecycle contract
/// accepts and Storage v2's `int32` fields carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(into = "i64", try_from = "i64")]
pub(crate) struct ConditionInt(i32);

impl TryFrom<i64> for ConditionInt {
    type Error = InvalidConditionInt;

    fn try_from(value: i64) -> Result<Self, InvalidConditionInt> {
        i32::try_from(value)
            .ok()
            .filter(|value| *value >= 0)
            .map(Self)
            .ok_or(InvalidConditionInt)
    }
}

impl From<ConditionInt> for i32 {
    fn from(value: ConditionInt) -> Self {
        value.0
    }
}

impl From<ConditionInt> for i64 {
    fn from(value: ConditionInt) -> Self {
        Self::from(value.0)
    }
}

impl From<ConditionInt> for u64 {
    fn from(value: ConditionInt) -> Self {
        Self::from(value.0.unsigned_abs())
    }
}

/// A count outside the nonnegative int32 range.
#[derive(Debug, thiserror::Error)]
#[error("Invalid lifecycle condition count")]
pub(crate) struct InvalidConditionInt;

/// A lifecycle condition date, 0001-01-01 to 9999-12-31 in the proleptic Gregorian calendar,
/// written `YYYY-MM-DD`. `FromStr` is the one calendar rule, shared by input validation and
/// stored rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleDate {
    year: u16,
    month: u8,
    day: u8,
}

impl LifecycleDate {
    pub(crate) fn year(self) -> i32 {
        i32::from(self.year)
    }

    pub(crate) fn month(self) -> i32 {
        i32::from(self.month)
    }

    pub(crate) fn day(self) -> i32 {
        i32::from(self.day)
    }
}

impl FromStr for LifecycleDate {
    type Err = InvalidLifecycleDate;

    fn from_str(value: &str) -> Result<Self, InvalidLifecycleDate> {
        let bytes = value.as_bytes();
        if bytes.len() != 10
            || bytes.get(4) != Some(&b'-')
            || bytes.get(7) != Some(&b'-')
            || bytes
                .iter()
                .enumerate()
                .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
        {
            return Err(InvalidLifecycleDate);
        }
        let part = |range: std::ops::Range<usize>| value.get(range).ok_or(InvalidLifecycleDate);
        let year: u16 = part(0..4)?.parse().map_err(|_| InvalidLifecycleDate)?;
        let month: u8 = part(5..7)?.parse().map_err(|_| InvalidLifecycleDate)?;
        let day: u8 = part(8..10)?.parse().map_err(|_| InvalidLifecycleDate)?;
        let days = crate::time_format::civil::days_in_month(i64::from(year), i64::from(month))
            .ok_or(InvalidLifecycleDate)?;
        if year == 0 || day == 0 || i64::from(day) > days {
            return Err(InvalidLifecycleDate);
        }
        Ok(Self { year, month, day })
    }
}

/// Text that is not a `YYYY-MM-DD` calendar date.
#[derive(Debug, thiserror::Error)]
#[error("Invalid lifecycle date")]
pub(crate) struct InvalidLifecycleDate;

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
        <String as serde::Deserialize>::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Lifecycle rules the lifecycle contract accepted. Built only by `from_json` (adapter input),
/// `from_values` (a JSON rule list) and `from_stored` (a catalog row), so a holder never
/// validates again.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct LifecycleRules(Vec<LifecycleRule>);

impl LifecycleRules {
    /// Parses a JSON `lifecycle` member; null or a missing `rule` is no rules. Ruby's nulls and
    /// empty lists are dropped and unknown condition members are dropped, while
    /// `matchesPattern` stays for validation to refuse.
    pub(crate) fn from_json(value: &Value) -> Result<Self, CatalogError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let config = value
            .as_object()
            .ok_or_else(|| invalid("configuration", "{\"rule\":[]}"))?;
        let Some(rules) = config.get("rule").filter(|value| !value.is_null()) else {
            return Ok(Self::default());
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
                // Cloud Storage dropped an unknown condition field and kept the rest of the rule
                // (recorded 2026-11). `matchesPattern` is documented but unsupported, so it stays
                // for `validate` to refuse rather than vanish.
                condition.retain(|field, value| {
                    if !CONDITIONS.contains(&field.as_str()) {
                        return field == "matchesPattern";
                    }
                    !(value.is_null()
                        || (matches!(
                            field.as_str(),
                            "matchesPrefix" | "matchesSuffix" | "matchesStorageClass"
                        ) && value.as_array().is_some_and(Vec::is_empty)))
                });
            }
        }
        // The typed rules ignore unknown action fields and canonicalize int64 strings.
        Self::from_values(&rules)
    }

    /// Validates a JSON rule list as input.
    pub(crate) fn from_values(values: &[Value]) -> Result<Self, CatalogError> {
        validate(values)?;
        deserialize_rules(values)
            .map(Self)
            .map_err(|error| invalid_rule(&error))
    }

    /// Decodes stored values with the input contract; a refusal is corrupt catalog metadata.
    pub(crate) fn from_stored(values: &[Value]) -> Result<Self, CatalogError> {
        decode_stored(values)
            .map(Self)
            .map_err(CatalogError::Corrupt)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn into_vec(self) -> Vec<LifecycleRule> {
        self.0
    }
}

impl<'a> IntoIterator for &'a LifecycleRules {
    type Item = &'a LifecycleRule;
    type IntoIter = std::slice::Iter<'a, LifecycleRule>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
impl LifecycleRules {
    /// Rules a test writes as JSON, which must satisfy the input contract.
    #[track_caller]
    pub(crate) fn for_test(values: Vec<Value>) -> Self {
        Self::from_values(&values).expect("valid test rules")
    }

    /// The JSON values these rules store, one per rule.
    pub(crate) fn to_values(&self) -> Vec<Value> {
        self.0
            .iter()
            .map(|rule| serde_json::to_value(rule).expect("typed rules serialize"))
            .collect()
    }
}

/// Stored values must satisfy the input contract before conversions narrow integers.
fn decode_stored(values: &[Value]) -> Result<Vec<LifecycleRule>, BackendCause> {
    validate(values).map_err(BackendCause::from_error)?;
    deserialize_rules(values).map_err(BackendCause::from_error)
}

/// Lifecycle rules as a bucket record carries them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BucketLifecycle {
    Rules(LifecycleRules),
    /// A stored value the current contract refuses: hand-edited, or written before a rule
    /// tightened. The bucket stays readable and a PATCH can replace it, while the scheduler
    /// quarantines it; JSON returns it as stored.
    Unreadable {
        values: Vec<Value>,
        cause: BackendCause,
    },
}

impl Default for BucketLifecycle {
    fn default() -> Self {
        Self::Rules(LifecycleRules::default())
    }
}

impl From<LifecycleRules> for BucketLifecycle {
    fn from(rules: LifecycleRules) -> Self {
        Self::Rules(rules)
    }
}

impl BucketLifecycle {
    pub(crate) fn from_stored(values: Vec<Value>) -> Self {
        match decode_stored(&values) {
            Ok(rules) => Self::Rules(LifecycleRules(rules)),
            Err(cause) => Self::Unreadable { values, cause },
        }
    }

    /// The rules, or the corrupt-metadata error a stored refusal reports.
    pub(crate) fn rules(&self) -> Result<&LifecycleRules, CatalogError> {
        match self {
            Self::Rules(rules) => Ok(rules),
            Self::Unreadable { cause, .. } => Err(CatalogError::Corrupt(cause.clone())),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Rules(rules) => rules.is_empty(),
            Self::Unreadable { values, .. } => values.is_empty(),
        }
    }
}

impl serde::Serialize for BucketLifecycle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Rules(rules) => rules.serialize(serializer),
            Self::Unreadable { values, .. } => values.serialize(serializer),
        }
    }
}

fn deserialize_rules(values: &[Value]) -> Result<Vec<LifecycleRule>, serde_json::Error> {
    values.iter().map(LifecycleRule::deserialize).collect()
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
    StorageClass::from_name(value)
        .map(|_| ())
        .map_err(|error| match error {
            InvalidStorageClass::RequiresZone => {
                CatalogError::Metadata(MetadataError::LifecycleClass {
                    field: field.into(),
                    value: value.into(),
                })
            }
            InvalidStorageClass::Unknown => {
                invalid(field, "a supported bucket class, for example COLDLINE")
            }
        })
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
        let kind = action.get("type").and_then(Value::as_str).ok_or_else(|| {
            invalid(
                "action.type",
                "Delete, SetStorageClass or AbortIncompleteMultipartUpload",
            )
        })?;
        if !matches!(
            kind,
            "Delete" | "SetStorageClass" | "AbortIncompleteMultipartUpload"
        ) {
            return Err(CatalogError::Metadata(MetadataError::LifecycleActionType(
                kind.into(),
            )));
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
                return Err(CatalogError::Metadata(
                    MetadataError::LifecycleAbortCondition,
                ));
            }
            prefixes_suffixes += validate_condition(field, value, rule_index)?;
        }
        if count == 0 {
            return Err(CatalogError::Metadata(
                MetadataError::LifecycleEmptyCondition,
            ));
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
                .is_some_and(|number| ConditionInt::try_from(number).is_ok())
            {
                return Err(invalid(field, "a nonnegative int32, for example 30"));
            }
        }
        "createdBefore" | "customTimeBefore" | "noncurrentTimeBefore" => {
            if !value
                .as_str()
                .is_some_and(|text| text.parse::<LifecycleDate>().is_ok())
            {
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
                    validate_class(
                        value,
                        &format!("rule[{rule_index}].condition.matchesStorageClass"),
                    )?;
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

impl LifecycleAction {
    const fn storage_class(self) -> Option<StorageClass> {
        match self {
            Self::SetStorageClass(class) => Some(class),
            Self::Delete | Self::AbortIncompleteMultipartUpload => None,
        }
    }

    /// Whether the action can run in a bucket of `location`. Configuration refuses a class the
    /// location cannot hold, so only a rule stored before that refusal existed fails this.
    pub(crate) fn runs_in(self, location: LocationKind) -> bool {
        self.storage_class()
            .is_none_or(|class| class.fits(location))
    }
}

impl LifecycleRule {
    /// The classes the rule names: its SetStorageClass target and the classes it matches.
    fn storage_classes(&self) -> impl Iterator<Item = StorageClass> + '_ {
        self.action
            .storage_class()
            .into_iter()
            .chain(self.condition.matches_storage_class.iter().copied())
    }
}

/// Refuses a rule naming a class the bucket's location cannot hold, as its action or among the
/// classes it matches, as Cloud Storage refuses every write of that class (recorded 2026-11-17).
pub(crate) fn validate_location(
    rules: &LifecycleRules,
    location: Location,
    project: &str,
) -> Result<(), CatalogError> {
    rules
        .into_iter()
        .flat_map(LifecycleRule::storage_classes)
        .try_for_each(|class| ClassLocationConflict::check(class, location, project))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::assert_matches;

    /// The values a bucket stores for the JSON `lifecycle` member `value`.
    fn parse(value: &Value) -> Result<Vec<Value>, CatalogError> {
        LifecycleRules::from_json(value).map(|rules| rules.to_values())
    }

    #[test]
    fn typed_lifecycle_preserves_all_actions_and_legacy_class_conditions() {
        for action in [
            json!({"type":"Delete"}),
            json!({"type":"AbortIncompleteMultipartUpload"}),
            json!({"type":"SetStorageClass","storageClass":"NEARLINE"}),
        ] {
            let stored = vec![json!({"action":action,"condition":{"age":1}})];
            let typed = LifecycleRules::from_stored(&stored).expect("existing action");
            assert_eq!(
                serde_json::to_string(&typed.to_values()).unwrap(),
                serde_json::to_string(&stored).unwrap()
            );
        }
        let stored = vec![
            json!({"action":{"type":"Delete"},"condition":{"matchesStorageClass":["REGIONAL","DURABLE_REDUCED_AVAILABILITY","MULTI_REGIONAL"]}}),
        ];
        let typed = LifecycleRules::from_stored(&stored).expect("legacy class matches");
        assert_eq!(
            serde_json::to_string(&typed.to_values()).unwrap(),
            serde_json::to_string(&stored).unwrap()
        );
    }

    #[test]
    fn typed_lifecycle_preserves_every_condition_and_rejects_invalid_values() {
        let values = vec![
            json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":30,"createdBefore":"2026-01-01","customTimeBefore":"2026-02-01","daysSinceCustomTime":2,"daysSinceNoncurrentTime":3,"isLive":false,"matchesPrefix":["a/"],"matchesStorageClass":["STANDARD"],"matchesSuffix":[".txt"],"noncurrentTimeBefore":"2026-03-01","numNewerVersions":4}}),
        ];
        let typed = LifecycleRules::from_stored(&values).unwrap();
        assert_eq!(
            serde_json::to_string(&typed.to_values()).unwrap(),
            serde_json::to_string(&values).unwrap()
        );
        // A typed rule cannot hold a count or date the contract refuses; parsing refuses them.
        for (age, accepted) in [
            (-1, false),
            (0, true),
            (i64::from(i32::MAX), true),
            (i64::from(i32::MAX) + 1, false),
        ] {
            assert_eq!(ConditionInt::try_from(age).is_ok(), accepted, "{age}");
            let rule = json!({"action":{"type":"Delete"},"condition":{"age":age}});
            match LifecycleRules::from_values(&[rule]) {
                Ok(rules) => {
                    assert_eq!(rules.into_vec()[0].condition.age.map(i64::from), Some(age))
                }
                Err(error) => {
                    assert!(!accepted, "{age}");
                    assert!(error.to_string().contains("nonnegative int32"), "{error}");
                }
            }
        }
        assert!("2025-02-29".parse::<LifecycleDate>().is_err());
        assert!(serde_json::from_value::<LifecycleDate>(json!("2025-02-29")).is_err());
        assert!(
            LifecycleRules::from_values(&[
                json!({"action":{"type":"Delete"},"condition":{"createdBefore":"2025-02-29"}})
            ])
            .unwrap_err()
            .to_string()
            .contains("valid YYYY-MM-DD")
        );
        assert_matches!(
            LifecycleRules::from_stored(&[json!({})]),
            Err(CatalogError::Corrupt(_))
        );
    }

    #[test]
    fn lifecycle_reads_preserve_stored_key_order_and_new_writes_are_canonical() {
        let original = r#"[{"condition":{"isLive":true,"age":1},"action":{"type":"Delete"}}]"#;
        let stored: Vec<Value> = serde_json::from_str(original).unwrap();
        assert_eq!(serde_json::to_string(&stored).unwrap(), original);
        // A row in another key order still reads as rules, and rules serialize canonically.
        let read = BucketLifecycle::from_stored(stored.clone());
        let BucketLifecycle::Rules(typed) = &read else {
            panic!("a valid stored rule reads as rules: {read:?}");
        };
        assert_eq!(typed.to_values(), stored);
        let canonical_json =
            r#"[{"action":{"type":"Delete"},"condition":{"age":1,"isLive":true}}]"#;
        assert_eq!(serde_json::to_string(&read).unwrap(), canonical_json);
        let parsed = parse(&json!({"rule":stored})).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), canonical_json);
        // A value the contract refuses stays as stored, and asking for its rules is corruption.
        let unreadable = vec![json!({"condition":{"age":-1},"action":{"type":"Delete"}})];
        let read = BucketLifecycle::from_stored(unreadable.clone());
        assert_eq!(serde_json::to_value(&read).unwrap(), json!(unreadable));
        assert_matches!(read.rules(), Err(CatalogError::Corrupt(_)));
        assert!(!read.is_empty());
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
        assert_matches!(
            LifecycleRules::from_values(&invalid),
            Err(CatalogError::Metadata(_))
        );
        assert_matches!(
            parse(&json!({"rule":invalid})),
            Err(CatalogError::Metadata(_))
        );
        assert_matches!(
            LifecycleRules::from_stored(&invalid),
            Err(CatalogError::Corrupt(_))
        );
    }

    #[test]
    fn lifecycle_input_drops_unknown_condition_fields_and_uses_provider_messages() {
        let parsed = parse(&json!({"rule":[{"action":{"type":"Delete"},
            "condition":{"age":1,"gcsmimicUnknown":{"nested":true}}}]}))
        .unwrap();
        assert_eq!(
            parsed,
            [json!({"action":{"type":"Delete"},"condition":{"age":1}})]
        );
        // A rule left without a known condition is still an empty condition.
        let message = |rule: Value| parse(&json!({"rule":[rule]})).unwrap_err().to_string();
        for condition in [json!({}), json!({"gcsmimicUnknown":1})] {
            assert_eq!(
                message(json!({"action":{"type":"Delete"},"condition":condition})),
                "Lifecycle rules must have a condition."
            );
        }
        assert_eq!(
            message(json!({"action":{"type":"Archive"},"condition":{"age":1}})),
            "Invalid value for: Archive is not a valid value"
        );
        assert_eq!(
            message(json!({"action":{"type":"AbortIncompleteMultipartUpload"},
                "condition":{"age":1,"isLive":true}})),
            "Lifecycle age_days, matches_prefix, and matches_suffix conditions are the only ones allowed for a ABORT_INCOMPLETE_MULTIPART_UPLOAD rule."
        );
        // Stored rules are not normalized, so an unknown field there is still corrupt.
        let stored = [json!({"action":{"type":"Delete"},"condition":{"age":1,"unknown":1}})];
        assert_matches!(
            LifecycleRules::from_stored(&stored),
            Err(CatalogError::Corrupt(_))
        );
        assert_matches!(
            parse(&json!({"rule":[{"action":{"type":"Delete"},
                "condition":{"matchesPattern":"*.txt"}}]})),
            Err(CatalogError::UnsupportedMetadata(_))
        );
    }

    #[test]
    fn lifecycle_date_and_prefix_suffix_boundaries() {
        for date in ["2000-02-29", "2024-02-29", "0001-01-01", "9999-12-31"] {
            let parsed: LifecycleDate = date.parse().unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(date));
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
            assert!(date.parse::<LifecycleDate>().is_err(), "{date}");
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
            "REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            validate(&[json!({"action":{"type":"SetStorageClass","storageClass":class},"condition":{"age":0}})]).unwrap();
        }
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let cases: &[(&dyn std::error::Error, &str)] = &[
        (&InvalidConditionInt, "Invalid lifecycle condition count"),
        (&InvalidLifecycleDate, "Invalid lifecycle date"),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), *expected);
        assert!(error.source().is_none());
    }
}
