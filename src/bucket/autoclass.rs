//! Transport-independent Autoclass configuration.
//!
//! Only the configuration contract is modeled here: enablement, the terminal storage class and
//! the server-computed timestamps. Access-driven class transitions are not simulated, so every
//! object in an enabled bucket stays in STANDARD storage.

use super::{
    CatalogError, StorageClass,
    lifecycle::{LifecycleAction, LifecycleRules},
};
use crate::metadata_error::MetadataError;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The bucket default class and newly written objects are always STANDARD while enabled.
pub(crate) const AUTOCLASS_STORAGE_CLASS: StorageClass = StorageClass::Standard;
const DEFAULT_TERMINAL_STORAGE_CLASS: TerminalStorageClass = TerminalStorageClass::Nearline;
/// The official Autoclass page forbids re-enabling until one day after it was disabled.
const REENABLE_DELAY: Duration = Duration::from_secs(24 * 60 * 60);

crate::choice::string_enum! {
    table
    /// The coldest class Autoclass moves objects to.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
    #[serde(try_from = "String")]
    pub(crate) enum TerminalStorageClass as pub(crate) as_str {
        Nearline => "NEARLINE",
        Archive => "ARCHIVE",
    };
    from_str(CatalogError, _ => Err(CatalogError::Metadata(MetadataError::AutoclassTerminal)));
    serde(try_from_string)
}

/// Stored configuration. A disabled configuration is kept so `toggleTime` records when
/// Autoclass was turned off; buckets that never enabled Autoclass have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(into = "StoredAutoclass", try_from = "StoredAutoclass")]
pub(crate) struct Autoclass {
    pub toggle_time: SystemTime,
    pub state: AutoclassState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoclassState {
    Disabled,
    Enabled {
        terminal_storage_class: TerminalStorageClass,
        terminal_storage_class_update_time: SystemTime,
    },
}

impl Autoclass {
    pub(crate) fn is_enabled(&self) -> bool {
        match self.state {
            AutoclassState::Enabled { .. } => true,
            AutoclassState::Disabled => false,
        }
    }

    /// The configuration a bucket created at `time` reports: creation is its toggle time and,
    /// while enabled, its terminal class update time.
    pub(crate) fn created_at(self, time: SystemTime) -> Self {
        let state = match self.state {
            AutoclassState::Enabled {
                terminal_storage_class,
                ..
            } => AutoclassState::Enabled {
                terminal_storage_class,
                terminal_storage_class_update_time: time,
            },
            AutoclassState::Disabled => AutoclassState::Disabled,
        };
        Self {
            toggle_time: time,
            state,
        }
    }
}

/// The catalog column's JSON shape, which the former struct's derive defined. Kept field for
/// field (names, order, `null` for an absent class) so stored rows stay byte-identical.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredAutoclass {
    enabled: bool,
    toggle_time: SystemTime,
    terminal_storage_class: Option<TerminalStorageClass>,
    terminal_storage_class_update_time: Option<SystemTime>,
}

impl From<Autoclass> for StoredAutoclass {
    fn from(config: Autoclass) -> Self {
        let (enabled, terminal_storage_class, terminal_storage_class_update_time) =
            match config.state {
                AutoclassState::Disabled => (false, None, None),
                AutoclassState::Enabled {
                    terminal_storage_class,
                    terminal_storage_class_update_time,
                } => (
                    true,
                    Some(terminal_storage_class),
                    Some(terminal_storage_class_update_time),
                ),
            };
        Self {
            enabled,
            toggle_time: config.toggle_time,
            terminal_storage_class,
            terminal_storage_class_update_time,
        }
    }
}

/// Every writer stores an enabled configuration with its terminal class and the class's update
/// time, and a disabled one with neither, so a stored value of another shape is corrupt.
impl TryFrom<StoredAutoclass> for Autoclass {
    type Error = InconsistentAutoclass;

    fn try_from(stored: StoredAutoclass) -> Result<Self, InconsistentAutoclass> {
        let state = match (
            stored.enabled,
            stored.terminal_storage_class,
            stored.terminal_storage_class_update_time,
        ) {
            (true, Some(terminal_storage_class), Some(terminal_storage_class_update_time)) => {
                AutoclassState::Enabled {
                    terminal_storage_class,
                    terminal_storage_class_update_time,
                }
            }
            (false, None, None) => AutoclassState::Disabled,
            (true, None, _) | (true, _, None) | (false, Some(_), _) | (false, _, Some(_)) => {
                return Err(InconsistentAutoclass);
            }
        };
        Ok(Self {
            toggle_time: stored.toggle_time,
            state,
        })
    }
}

/// A stored configuration whose enablement disagrees with its terminal class fields.
#[derive(Debug)]
pub(crate) struct InconsistentAutoclass;

impl std::fmt::Display for InconsistentAutoclass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("stored autoclass enablement disagrees with its terminal storage class")
    }
}

impl std::error::Error for InconsistentAutoclass {}

/// A requested change. `None` fields keep the current value under patch semantics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutoclassUpdate {
    pub enabled: Option<bool>,
    pub terminal_storage_class: Option<TerminalStorageClass>,
}

fn invalid(reason: &str) -> CatalogError {
    CatalogError::InvalidMetadata(reason.into())
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
                        value
                            .as_str()
                            .ok_or_else(|| {
                                invalid(
                                    "autoclass.terminalStorageClass must be NEARLINE or ARCHIVE",
                                )
                            })?
                            .parse()
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
        Ok(duration) => {
            UNIX_EPOCH + duration - Duration::from_nanos(u64::from(duration.subsec_nanos() % 1000))
        }
        Err(_) => time,
    }
}

/// Apply `update` to `current`; omitted subfields keep their current values. Whether the
/// request turned Autoclass on follows from comparing `current` with the result.
///
/// PATCH and PUT share this merge rule because Ruby's `Bucket#update` sends PUT bodies with
/// only the changed attributes, and the local PUT contract already preserves omitted fields.
pub(crate) fn apply(
    current: Option<&Autoclass>,
    update: AutoclassUpdate,
    now: SystemTime,
) -> Result<Option<Autoclass>, CatalogError> {
    let now = floor_micros(now);
    let currently_enabled = current.is_some_and(Autoclass::is_enabled);
    if !update.enabled.unwrap_or(currently_enabled) {
        if update.terminal_storage_class.is_some() {
            return Err(CatalogError::Metadata(MetadataError::AutoclassDisabled));
        }
        return Ok(if currently_enabled {
            Some(Autoclass {
                toggle_time: now,
                state: AutoclassState::Disabled,
            })
        } else {
            current.copied()
        });
    }
    if let Some(current) = current
        && let AutoclassState::Enabled {
            terminal_storage_class,
            ..
        } = current.state
    {
        let state = match update.terminal_storage_class {
            Some(terminal) if terminal != terminal_storage_class => AutoclassState::Enabled {
                terminal_storage_class: terminal,
                terminal_storage_class_update_time: now,
            },
            Some(_) | None => current.state,
        };
        return Ok(Some(Autoclass {
            toggle_time: current.toggle_time,
            state,
        }));
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
    Ok(Some(Autoclass {
        toggle_time: now,
        state: AutoclassState::Enabled {
            terminal_storage_class: update
                .terminal_storage_class
                .unwrap_or(DEFAULT_TERMINAL_STORAGE_CLASS),
            terminal_storage_class_update_time: now,
        },
    }))
}

/// Reject final bucket states that the official Autoclass restrictions forbid.
pub(crate) fn validate_bucket(
    autoclass: Option<&Autoclass>,
    storage_class: StorageClass,
    lifecycle: &LifecycleRules,
) -> Result<(), CatalogError> {
    if !autoclass.is_some_and(Autoclass::is_enabled) {
        return Ok(());
    }
    if storage_class != AUTOCLASS_STORAGE_CLASS {
        return Err(CatalogError::Metadata(MetadataError::AutoclassStorageClass));
    }
    if lifecycle.into_iter().any(|rule| {
        matches!(rule.action, LifecycleAction::SetStorageClass(_))
            || !rule.condition.matches_storage_class.is_empty()
    }) {
        return Err(CatalogError::Metadata(MetadataError::AutoclassLifecycle));
    }
    Ok(())
}

/// Storage class for a newly written object. Autoclass ignores the requested class.
pub(crate) fn object_storage_class(
    autoclass: Option<&Autoclass>,
    requested: Option<StorageClass>,
    bucket_default: StorageClass,
) -> StorageClass {
    if autoclass.is_some_and(Autoclass::is_enabled) {
        AUTOCLASS_STORAGE_CLASS
    } else {
        requested.unwrap_or(bucket_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_storage_class_spellings_and_json_are_stable() {
        for (class, spelling) in [
            (TerminalStorageClass::Nearline, "NEARLINE"),
            (TerminalStorageClass::Archive, "ARCHIVE"),
        ] {
            let json = format!("{spelling:?}");
            assert_eq!(spelling.parse::<TerminalStorageClass>().unwrap(), class);
            assert_eq!(serde_json::to_string(&class).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<TerminalStorageClass>(&json).unwrap(),
                class
            );
        }
        for rejected in ["nearline", "STANDARD", "ARCHIVE "] {
            assert_eq!(
                rejected
                    .parse::<TerminalStorageClass>()
                    .unwrap_err()
                    .to_string(),
                "Invalid value for terminalStorageClass field in Autoclass config. Valid values are NEARLINE and ARCHIVE"
            );
        }
    }

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
    }

    fn enable(class: Option<TerminalStorageClass>) -> AutoclassUpdate {
        AutoclassUpdate {
            enabled: Some(true),
            terminal_storage_class: class,
        }
    }

    /// The enabled state with `class` updated at `time`.
    fn enabled(class: TerminalStorageClass, time: SystemTime) -> AutoclassState {
        AutoclassState::Enabled {
            terminal_storage_class: class,
            terminal_storage_class_update_time: time,
        }
    }

    #[test]
    fn autoclass_json_parsing_validates_fields() {
        assert_eq!(
            parse_json(&json!({"enabled":true,"terminalStorageClass":"ARCHIVE","toggleTime":"x"}))
                .unwrap(),
            enable(Some(TerminalStorageClass::Archive))
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
            let error = parse_json(&bad).unwrap_err();
            if bad["terminalStorageClass"].is_string() {
                assert_eq!(
                    error,
                    CatalogError::Metadata(MetadataError::AutoclassTerminal)
                );
                assert_eq!(
                    crate::api_error::ApiError::from(error).message,
                    "Invalid value for terminalStorageClass field in Autoclass config. Valid values are NEARLINE and ARCHIVE"
                );
            } else {
                assert!(matches!(error, CatalogError::InvalidMetadata(_)), "{bad}");
            }
        }
    }

    #[test]
    fn autoclass_enable_sets_server_times_and_default_terminal_class() {
        let config = apply(None, enable(None), at(0)).unwrap().unwrap();
        assert_eq!(config.toggle_time, at(0));
        assert_eq!(config.state, enabled(TerminalStorageClass::Nearline, at(0)));

        let patched = apply(
            Some(&config),
            enable(Some(TerminalStorageClass::Archive)),
            at(5),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            patched.toggle_time,
            at(0),
            "toggleTime only tracks enablement"
        );
        assert_eq!(patched.state, enabled(TerminalStorageClass::Archive, at(5)));

        let same = apply(
            Some(&patched),
            enable(Some(TerminalStorageClass::Archive)),
            at(9),
        )
        .unwrap();
        assert_eq!(same, Some(patched), "unchanged class keeps update time");
        for omitted in [AutoclassUpdate::default(), enable(None)] {
            let kept = apply(Some(&patched), omitted, at(9)).unwrap();
            assert_eq!(kept, Some(patched), "omitted subfields are kept");
        }
    }

    #[test]
    fn autoclass_disable_and_reenable_boundary() {
        let disable = AutoclassUpdate {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(
            apply(None, disable, at(0)).unwrap(),
            None,
            "disabling an unconfigured bucket stays unconfigured"
        );
        let config = apply(None, enable(Some(TerminalStorageClass::Archive)), at(0)).unwrap();
        let disabled = apply(config.as_ref(), disable, at(10)).unwrap().unwrap();
        assert_eq!(
            disabled,
            Autoclass {
                toggle_time: at(10),
                state: AutoclassState::Disabled
            }
        );
        let still = apply(Some(&disabled), disable, at(20)).unwrap();
        assert_eq!(still, Some(disabled), "repeat disable keeps toggleTime");

        let just_before = at(10 + 86_399);
        assert!(matches!(
            apply(Some(&disabled), enable(None), just_before),
            Err(CatalogError::InvalidMetadata(_))
        ));
        let reenabled = apply(Some(&disabled), enable(None), at(10 + 86_400))
            .unwrap()
            .unwrap();
        assert_eq!(reenabled.toggle_time, at(10 + 86_400));
        assert_eq!(
            reenabled.state,
            enabled(TerminalStorageClass::Nearline, at(10 + 86_400))
        );

        assert!(matches!(
            apply(
                Some(&disabled),
                AutoclassUpdate {
                    enabled: None,
                    terminal_storage_class: Some(TerminalStorageClass::Archive)
                },
                at(20)
            ),
            Err(CatalogError::Metadata(MetadataError::AutoclassDisabled))
        ));
    }

    #[test]
    fn autoclass_bucket_restrictions() {
        let config = apply(None, enable(None), at(0)).unwrap();
        let config = config.as_ref();
        validate_bucket(
            config,
            StorageClass::Standard,
            &LifecycleRules::from_values(&[
                json!({"action":{"type":"Delete"},"condition":{"age":1}}),
            ])
            .unwrap(),
        )
        .unwrap();
        for class in ["NEARLINE", "COLDLINE", "ARCHIVE"] {
            assert!(
                validate_bucket(config, class.parse().unwrap(), &LifecycleRules::default())
                    .is_err()
            );
            validate_bucket(None, class.parse().unwrap(), &LifecycleRules::default()).unwrap();
        }
        for rule in [
            json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}}),
            json!({"action":{"type":"Delete"},"condition":{"matchesStorageClass":["STANDARD"]}}),
        ] {
            let rules = LifecycleRules::from_values(&[rule]).unwrap();
            assert!(validate_bucket(config, StorageClass::Standard, &rules).is_err());
            validate_bucket(None, StorageClass::Standard, &rules).unwrap();
        }
        assert_eq!(
            object_storage_class(config, Some(StorageClass::Coldline), StorageClass::Standard),
            StorageClass::Standard
        );
        assert_eq!(
            object_storage_class(None, Some(StorageClass::Coldline), StorageClass::Standard),
            StorageClass::Coldline
        );
        assert_eq!(
            object_storage_class(None, None, StorageClass::Nearline),
            StorageClass::Nearline
        );
    }

    /// The column keeps the former derive's text for both states, and refuses every shape in
    /// which enablement and the terminal class fields disagree.
    #[test]
    fn stored_autoclass_shapes_round_trip_and_inconsistent_ones_are_refused() {
        let time = r#"{"secs_since_epoch":1700000000,"nanos_since_epoch":123456000}"#;
        let toggle_time = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_000);
        for (config, text) in [
            (
                Autoclass {
                    toggle_time,
                    state: enabled(TerminalStorageClass::Archive, toggle_time),
                },
                format!(
                    r#"{{"enabled":true,"toggle_time":{time},"terminal_storage_class":"ARCHIVE","terminal_storage_class_update_time":{time}}}"#
                ),
            ),
            (
                Autoclass {
                    toggle_time,
                    state: AutoclassState::Disabled,
                },
                format!(
                    r#"{{"enabled":false,"toggle_time":{time},"terminal_storage_class":null,"terminal_storage_class_update_time":null}}"#
                ),
            ),
        ] {
            assert_eq!(serde_json::to_string(&config).unwrap(), text);
            assert_eq!(serde_json::from_str::<Autoclass>(&text).unwrap(), config);
        }
        for (enabled, class, update_time) in [
            ("true", "null", time),
            ("true", r#""NEARLINE""#, "null"),
            ("false", r#""NEARLINE""#, "null"),
            ("false", "null", time),
            ("true", r#""COLDLINE""#, time),
        ] {
            let text = format!(
                r#"{{"enabled":{enabled},"toggle_time":{time},"terminal_storage_class":{class},"terminal_storage_class_update_time":{update_time}}}"#
            );
            assert!(serde_json::from_str::<Autoclass>(&text).is_err(), "{text}");
        }
    }
}
