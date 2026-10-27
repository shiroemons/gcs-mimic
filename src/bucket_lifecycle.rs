//! Validated lifecycle metadata; execution belongs to the maintenance scheduler.

use super::{CatalogError, validate_storage_class};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn invalid(field: &str, example: &str) -> CatalogError {
    CatalogError::InvalidMetadata(format!("Invalid lifecycle {field}; use {example}"))
}

fn validate_class(value: &str, field: &str) -> Result<(), CatalogError> {
    validate_storage_class(value).map_err(|error| match error {
        CatalogError::UnsupportedMetadata(reason) => {
            CatalogError::UnsupportedMetadata(format!("lifecycle.{field}={value}: {reason}"))
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
    validate(&rules)?;
    // Unknown input fields follow the bucket adapter's ignored-input contract.
    Ok(rules
        .iter()
        .map(|rule| {
            let action = &rule["action"];
            let mut action_out = json!({"type": action["type"]});
            if let Some(class) = action.get("storageClass") {
                action_out["storageClass"] = class.clone();
            }
            json!({"action": action_out, "condition": rule["condition"]})
        })
        .collect())
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
];

pub(crate) fn validate(rules: &[Value]) -> Result<(), CatalogError> {
    if rules.len() > 100 {
        return Err(invalid("rule count (maximum 100)", "100 or fewer rules"));
    }
    if serde_json::to_string(rules)
        .map_err(|error| invalid("rules", &error.to_string()))?
        .len()
        > 64 * 1024
    {
        return Err(invalid("configuration size", "at most 64 KiB"));
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
                "storageClass only with SetStorageClass",
            ));
        }
        let condition = rule
            .get("condition")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("condition", "{\"age\":30}"))?;
        let mut count = 0;
        for (field, value) in condition {
            if matches!(
                field.as_str(),
                "matchesPattern" | "sizeAboveBytes" | "sizeBelowBytes"
            ) {
                return Err(CatalogError::UnsupportedMetadata(format!(
                    "lifecycle.condition.{field}"
                )));
            }
            if !CONDITIONS.contains(&field.as_str()) {
                return Err(invalid(
                    field,
                    "a documented lifecycle condition, for example age",
                ));
            }
            count += 1;
            if kind == "AbortIncompleteMultipartUpload"
                && !matches!(field.as_str(), "age" | "matchesPrefix" | "matchesSuffix")
            {
                return Err(invalid(
                    "AbortIncompleteMultipartUpload condition",
                    "age, matchesPrefix or matchesSuffix",
                ));
            }
            match field.as_str() {
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
                        prefixes_suffixes += values.len();
                    }
                }
            }
        }
        if count == 0 {
            return Err(invalid(
                "empty condition",
                "at least one condition, for example {\"age\":30}",
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

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = value[..4].parse().unwrap();
    let month: u32 = value[5..7].parse().unwrap();
    let day: u32 = value[8..].parse().unwrap();
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

pub(crate) fn validate_location(rules: &[Value], location_type: &str) -> Result<(), CatalogError> {
    for rule in rules {
        if let Some(classes) = rule["condition"]["matchesStorageClass"].as_array() {
            let incompatible = if location_type == "region" {
                "MULTI_REGIONAL"
            } else {
                "REGIONAL"
            };
            if classes.iter().any(|class| class == incompatible) {
                return Err(invalid(
                    "matchesStorageClass for bucket location",
                    "STANDARD instead of a legacy class from another location type",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
