use super::{Config, ConfigurationReport, Diagnostic, Setting, SettingFix, Settings};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Disk,
    Memory,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Memory => "memory",
        }
    }
}

pub(super) fn apply(config: &mut Config, setting: Setting, value: &str) -> Result<(), SettingFix> {
    match setting {
        Setting::Backend => {
            config.backend = match value {
                "disk" | "filesystem" => Backend::Disk,
                "memory" => Backend::Memory,
                _ => return Err("backend must be disk or memory".into()),
            }
        }
        Setting::MemoryMaxBytes => config.memory_max_bytes = setting.integer(value)?,
        Setting::MemoryCatalogMaxBytes => {
            config.memory_catalog_max_bytes = setting.integer(value)?
        }
        _ => return Err(super::unknown_setting()),
    }
    Ok(())
}

impl ConfigurationReport {
    pub(super) fn check_backend(&mut self, settings: &Settings) {
        if self.config.backend != Backend::Memory {
            for key in ["memory_max_bytes", "memory_catalog_max_bytes"] {
                if let Some(input) = settings.get(key) {
                    self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                        "RAM byte limits require backend=memory; remove them or select the memory backend"));
                    self.invalid.insert(key.into());
                }
            }
            return;
        }
        for key in ["sync_dir", "sync_reinitialize"] {
            if let Some(input) = settings.get(key) {
                self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                    "backend=memory does not support directory sync; remove this setting or use backend=disk"));
                self.invalid.insert(key.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, num::NonZeroU64};

    #[test]
    fn lifecycle_execution_defaults_on_and_disable_obeys_native_precedence() {
        let data = crate::test_support::TestDir::new();
        let file = data.0.join("config.json");
        use super::super::LifecycleExecution::{Disabled, Enabled};
        std::fs::write(&file, br#"{"lifecycle_execution":"disabled"}"#).unwrap();
        super::super::test_support::assert_precedence(
            &["--config".into(), file.to_str().unwrap().into()],
            &BTreeMap::from([("GCS_MIMIC_LIFECYCLE_EXECUTION".into(), "enabled".into())]),
            &["--lifecycle-execution", "disabled"],
            [None, Some(Disabled), Some(Enabled), Some(Disabled)],
            Config::load,
            |config| config.lifecycle_execution,
        );
        super::super::test_support::assert_rejected(
            "lifecycle_execution",
            &["true", "1", ""],
            "enabled or disabled",
        );
    }

    #[test]
    fn every_backend_defaults_soft_delete_off_unless_the_setting_is_explicit() {
        let load = |args: &[&str], env: &[(&str, &str)]| {
            Config::load(
                &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
                &env.iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            )
            .map(|config| config.default_soft_delete_retention_seconds)
        };
        const FLAG: &str = "--default-soft-delete-retention-seconds";
        const ENV: &str = "GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS";
        for backend in ["disk", "memory"] {
            assert_eq!(load(&["--backend", backend], &[]).unwrap(), None);
            assert_eq!(
                load(&["--backend", backend], &[(ENV, "604800")]).unwrap(),
                NonZeroU64::new(604_800)
            );
            for seconds in [0, 1, 5, 1_209_600, 3_155_760_000_u64] {
                assert_eq!(
                    load(&["--backend", backend, FLAG, &seconds.to_string()], &[]).unwrap(),
                    NonZeroU64::new(seconds)
                );
            }
        }
        assert_eq!(load(&[], &[]).unwrap(), None);
        for rejected in ["3155760001", "18446744073709551616", "-1", "1.5", "week"] {
            assert!(load(&[FLAG, rejected], &[]).is_err(), "{rejected}");
        }
    }

    #[test]
    fn memory_defaults_aliases_budgets_and_unsupported_startup_options() {
        let config = Config::load(&["-backend".into(), "memory".into()], &BTreeMap::new()).unwrap();
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(
            config.memory_max_bytes,
            crate::memory_limit::default_budget()
        );
        assert_eq!(config.memory_catalog_max_bytes, 16 * 1024 * 1024);
        for args in [
            vec!["--backend", "unknown"],
            vec!["--backend", "memory", "--memory-max-bytes", "0"],
            vec!["--backend", "memory", "--memory-catalog-max-bytes", "1024"],
            vec!["--backend", "memory", "--sync-dir", "missing"],
        ] {
            assert!(
                Config::load(
                    &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
        let env = BTreeMap::from([
            ("GCS_MIMIC_BACKEND".into(), "memory".into()),
            ("GCS_MIMIC_MEMORY_MAX_BYTES".into(), "2000".into()),
        ]);
        let config = Config::load(&["--memory-max-bytes".into(), "3000".into()], &env).unwrap();
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(config.memory_max_bytes, 3000);
        let config = Config::load(
            &[
                "--backend".into(),
                "memory".into(),
                "--grpc-listen".into(),
                "127.0.0.1:0".into(),
            ],
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(config.grpc_listen.is_some());
        assert_eq!(config.backend, Backend::Memory);
    }
}
