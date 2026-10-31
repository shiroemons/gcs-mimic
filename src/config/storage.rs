use super::{Config, ConfigurationReport, Diagnostic, SettingFix, Settings};

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

pub(super) fn apply(config: &mut Config, key: &str, value: &str) -> Result<(), SettingFix> {
    match key {
        "backend" => {
            config.backend = match value {
                "disk" | "filesystem" => Backend::Disk,
                "memory" => Backend::Memory,
                _ => return Err("backend must be disk or memory".into()),
            }
        }
        "memory_max_bytes" => {
            config.memory_max_bytes = super::bounded(key, value, 1, 1024 * 1024 * 1024)?
        }
        "memory_catalog_max_bytes" => {
            config.memory_catalog_max_bytes =
                super::bounded(key, value, 1024 * 1024, 1024 * 1024 * 1024)?
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
    use std::collections::BTreeMap;

    #[test]
    fn memory_defaults_aliases_budgets_and_unsupported_startup_options() {
        let config = Config::load(&["-backend".into(), "memory".into()], &BTreeMap::new()).unwrap();
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(config.memory_max_bytes, 64 * 1024 * 1024);
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
