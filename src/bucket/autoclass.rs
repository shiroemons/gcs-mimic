//! Transport-independent Autoclass configuration.
//!
//! Only the configuration contract is modeled here: enablement, the terminal storage class and
//! the server-computed timestamps. Access-driven class transitions are not simulated, so every
//! object in an enabled bucket stays in STANDARD storage.

use super::CatalogError;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The bucket default class and newly written objects are always STANDARD while enabled.
pub(crate) const AUTOCLASS_STORAGE_CLASS: &str = "STANDARD";
const DEFAULT_TERMINAL_STORAGE_CLASS: &str = "NEARLINE";
/// The official Autoclass page forbids re-enabling until one day after it was disabled.
const REENABLE_DELAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Stored configuration. A disabled configuration is kept so `toggleTime` records when
/// Autoclass was turned off; buckets that never enabled Autoclass have no configuration.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Autoclass {
    pub enabled: bool,
    pub toggle_time: SystemTime,
    pub terminal_storage_class: Option<String>,
    pub terminal_storage_class_update_time: Option<SystemTime>,
}

/// A requested change. `None` fields keep the current value under patch semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoclassUpdate {
    pub enabled: Option<bool>,
    pub terminal_storage_class: Option<String>,
}

fn invalid(reason: &str) -> CatalogError {
    CatalogError::InvalidMetadata(reason.into())
}

pub(crate) fn validate_terminal_storage_class(class: &str) -> Result<(), CatalogError> {
    match class {
        "NEARLINE" | "ARCHIVE" => Ok(()),
        _ => Err(invalid(
            "Invalid autoclass.terminalStorageClass; use NEARLINE or ARCHIVE",
        )),
    }
}

/// Parse the JSON `autoclass` object. Output-only timestamps are ignored like other
/// server-managed bucket fields, so SDKs can send back a resource they previously read.
pub(crate) fn parse_json(value: &Value) -> Result<AutoclassUpdate, CatalogError> {
    if value.is_null() {
        // Clearing the configuration disables Autoclass.
        return Ok(AutoclassUpdate {
            enabled: Some(false),
            terminal_storage_class: None,
        });
    }
    let object = value.as_object().ok_or_else(|| {
        invalid("autoclass must be an object; use {\"enabled\":true,\"terminalStorageClass\":\"NEARLINE\"}")
    })?;
    let mut update = AutoclassUpdate::default();
    for (key, value) in object {
        match key.as_str() {
            "enabled" => {
                update.enabled = (!value.is_null())
                    .then(|| {
                        value.as_bool().ok_or_else(|| {
                            invalid("autoclass.enabled must be a boolean; use true or false")
                        })
                    })
                    .transpose()?;
            }
            "terminalStorageClass" => {
                update.terminal_storage_class = (!value.is_null())
                    .then(|| {
                        let class = value.as_str().ok_or_else(|| {
                            invalid("autoclass.terminalStorageClass must be NEARLINE or ARCHIVE")
                        })?;
                        validate_terminal_storage_class(class)?;
                        Ok::<_, CatalogError>(class.to_owned())
                    })
                    .transpose()?;
            }
            "toggleTime" | "terminalStorageClassUpdateTime" => {}
            _ => {
                return Err(invalid(&format!(
                    "Unknown autoclass field {key}; use enabled and terminalStorageClass"
                )));
            }
        }
    }
    Ok(update)
}

fn floor_micros(time: SystemTime) -> SystemTime {
    // The catalog stores bucket timestamps with microsecond precision.
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => UNIX_EPOCH + Duration::from_micros(duration.as_micros() as u64),
        Err(_) => time,
    }
}

/// Apply `update` to `current`; omitted subfields keep their current values.
///
/// PATCH and PUT share this merge rule because Ruby's `Bucket#update` sends PUT bodies with
/// only the changed attributes, and the local PUT contract already preserves omitted fields.
/// Returns the new configuration and whether this request turned Autoclass on.
pub(crate) fn apply(
    current: Option<&Autoclass>,
    update: &AutoclassUpdate,
    now: SystemTime,
) -> Result<(Option<Autoclass>, bool), CatalogError> {
    let now = floor_micros(now);
    let currently_enabled = current.is_some_and(|config| config.enabled);
    let enabled = update.enabled.unwrap_or(currently_enabled);
    if let Some(class) = &update.terminal_storage_class {
        validate_terminal_storage_class(class)?;
    }
    if !enabled {
        if update.terminal_storage_class.is_some() {
            return Err(invalid(
                "autoclass.terminalStorageClass requires autoclass.enabled=true",
            ));
        }
        return Ok((
            if currently_enabled {
                Some(Autoclass {
                    enabled: false,
                    toggle_time: now,
                    terminal_storage_class: None,
                    terminal_storage_class_update_time: None,
                })
            } else {
                current.cloned()
            },
            false,
        ));
    }
    if let Some(current) = current.filter(|config| config.enabled) {
        let mut next = current.clone();
        if let Some(terminal) = &update.terminal_storage_class
            && next.terminal_storage_class.as_ref() != Some(terminal)
        {
            next.terminal_storage_class = Some(terminal.clone());
            next.terminal_storage_class_update_time = Some(now);
        }
        return Ok((Some(next), false));
    }
    if let Some(disabled) = current
        && disabled
            .toggle_time
            .checked_add(REENABLE_DELAY)
            .is_some_and(|allowed| now < allowed)
    {
        return Err(invalid(
            "Autoclass cannot be re-enabled until one day after it was disabled; retry after autoclass.toggleTime plus 24 hours",
        ));
    }
    Ok((
        Some(Autoclass {
            enabled: true,
            toggle_time: now,
            terminal_storage_class: Some(
                update
                    .terminal_storage_class
                    .clone()
                    .unwrap_or_else(|| DEFAULT_TERMINAL_STORAGE_CLASS.to_owned()),
            ),
            terminal_storage_class_update_time: Some(now),
        }),
        true,
    ))
}

/// Reject final bucket states that the official Autoclass restrictions forbid.
pub(crate) fn validate_bucket(
    autoclass: Option<&Autoclass>,
    storage_class: &str,
    lifecycle: &[Value],
) -> Result<(), CatalogError> {
    if !autoclass.is_some_and(|config| config.enabled) {
        return Ok(());
    }
    if storage_class != AUTOCLASS_STORAGE_CLASS {
        return Err(invalid(
            "An Autoclass bucket's storageClass must be STANDARD; omit storageClass or use STANDARD",
        ));
    }
    if lifecycle.iter().any(|rule| {
        rule["action"]["type"] == "SetStorageClass"
            || rule["condition"].get("matchesStorageClass").is_some()
    }) {
        return Err(invalid(
            "Autoclass cannot be combined with lifecycle SetStorageClass actions or matchesStorageClass conditions; remove those rules",
        ));
    }
    Ok(())
}

/// Storage class for a newly written object. Autoclass ignores the requested class.
pub(crate) fn object_storage_class(
    autoclass: Option<&Autoclass>,
    requested: Option<String>,
    bucket_default: String,
) -> String {
    if autoclass.is_some_and(|config| config.enabled) {
        AUTOCLASS_STORAGE_CLASS.to_owned()
    } else {
        requested.unwrap_or(bucket_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
    }

    fn enable(class: Option<&str>) -> AutoclassUpdate {
        AutoclassUpdate {
            enabled: Some(true),
            terminal_storage_class: class.map(str::to_owned),
        }
    }

    #[test]
    fn autoclass_json_parsing_validates_fields() {
        assert_eq!(
            parse_json(&json!({"enabled":true,"terminalStorageClass":"ARCHIVE","toggleTime":"x"}))
                .unwrap(),
            enable(Some("ARCHIVE"))
        );
        assert_eq!(
            parse_json(&Value::Null).unwrap().enabled,
            Some(false),
            "null clears the configuration"
        );
        for bad in [
            json!(true),
            json!({"enabled":"yes"}),
            json!({"terminalStorageClass":"COLDLINE"}),
            json!({"terminalStorageClass":"STANDARD"}),
            json!({"terminalStorageClass":"nearline"}),
            json!({"terminalStorageClass":1}),
            json!({"enabledd":true}),
        ] {
            assert!(
                matches!(parse_json(&bad), Err(CatalogError::InvalidMetadata(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn autoclass_enable_sets_server_times_and_default_terminal_class() {
        let (config, enabled) = apply(None, &enable(None), at(0)).unwrap();
        let config = config.unwrap();
        assert!(enabled && config.enabled);
        assert_eq!(config.toggle_time, at(0));
        assert_eq!(config.terminal_storage_class.as_deref(), Some("NEARLINE"));
        assert_eq!(config.terminal_storage_class_update_time, Some(at(0)));

        let (patched, newly) = apply(Some(&config), &enable(Some("ARCHIVE")), at(5)).unwrap();
        let patched = patched.unwrap();
        assert!(!newly);
        assert_eq!(
            patched.toggle_time,
            at(0),
            "toggleTime only tracks enablement"
        );
        assert_eq!(patched.terminal_storage_class.as_deref(), Some("ARCHIVE"));
        assert_eq!(patched.terminal_storage_class_update_time, Some(at(5)));

        let (same, _) = apply(Some(&patched), &enable(Some("ARCHIVE")), at(9)).unwrap();
        assert_eq!(
            same.as_ref(),
            Some(&patched),
            "unchanged class keeps update time"
        );
        for omitted in [AutoclassUpdate::default(), enable(None)] {
            let (kept, newly) = apply(Some(&patched), &omitted, at(9)).unwrap();
            assert_eq!(kept.as_ref(), Some(&patched), "omitted subfields are kept");
            assert!(!newly);
        }
    }

    #[test]
    fn autoclass_disable_and_reenable_boundary() {
        let disable = AutoclassUpdate {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(
            apply(None, &disable, at(0)).unwrap(),
            (None, false),
            "disabling an unconfigured bucket stays unconfigured"
        );
        let (config, _) = apply(None, &enable(Some("ARCHIVE")), at(0)).unwrap();
        let (disabled, _) = apply(config.as_ref(), &disable, at(10)).unwrap();
        let disabled = disabled.unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.toggle_time, at(10));
        assert_eq!(disabled.terminal_storage_class, None);
        let (still, _) = apply(Some(&disabled), &disable, at(20)).unwrap();
        assert_eq!(
            still.as_ref(),
            Some(&disabled),
            "repeat disable keeps toggleTime"
        );

        let just_before = at(10 + 86_399);
        assert!(matches!(
            apply(Some(&disabled), &enable(None), just_before),
            Err(CatalogError::InvalidMetadata(_))
        ));
        let (reenabled, newly) = apply(Some(&disabled), &enable(None), at(10 + 86_400)).unwrap();
        let reenabled = reenabled.unwrap();
        assert!(newly && reenabled.enabled);
        assert_eq!(reenabled.toggle_time, at(10 + 86_400));
        assert_eq!(
            reenabled.terminal_storage_class.as_deref(),
            Some("NEARLINE")
        );

        assert!(matches!(
            apply(
                Some(&disabled),
                &AutoclassUpdate {
                    enabled: None,
                    terminal_storage_class: Some("ARCHIVE".into())
                },
                at(20)
            ),
            Err(CatalogError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn autoclass_bucket_restrictions() {
        let (config, _) = apply(None, &enable(None), at(0)).unwrap();
        let config = config.as_ref();
        validate_bucket(
            config,
            "STANDARD",
            &[json!({"action":{"type":"Delete"},"condition":{"age":1}})],
        )
        .unwrap();
        for class in ["NEARLINE", "COLDLINE", "ARCHIVE"] {
            assert!(validate_bucket(config, class, &[]).is_err());
            validate_bucket(None, class, &[]).unwrap();
        }
        for rule in [
            json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}}),
            json!({"action":{"type":"Delete"},"condition":{"matchesStorageClass":["STANDARD"]}}),
        ] {
            assert!(validate_bucket(config, "STANDARD", std::slice::from_ref(&rule)).is_err());
            validate_bucket(None, "STANDARD", &[rule]).unwrap();
        }
        assert_eq!(
            object_storage_class(config, Some("COLDLINE".into()), "STANDARD".into()),
            "STANDARD"
        );
        assert_eq!(
            object_storage_class(None, Some("COLDLINE".into()), "STANDARD".into()),
            "COLDLINE"
        );
        assert_eq!(
            object_storage_class(None, None, "NEARLINE".into()),
            "NEARLINE"
        );
    }
}
