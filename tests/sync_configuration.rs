#[path = "../src/test_support.rs"]
mod support;
use gcs_mimic::config::Config;
use std::{collections::BTreeMap, fs};
use support::TestDir;

fn settings(root: &TestDir) -> Vec<String> {
    fs::create_dir_all(root.0.join("tree")).unwrap();
    vec![
        "--sync-dir".into(),
        root.0.join("tree").to_str().unwrap().into(),
        "--data-dir".into(),
        root.0.join("data").to_str().unwrap().into(),
    ]
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn strict_synchronization_requires_a_registry_and_keeps_strict_authentication() {
    let root = TestDir::new();
    let mut args = settings(&root);
    args.extend(["--auth-mode".into(), "strict".into()]);
    assert!(
        Config::load(&args, &BTreeMap::new())
            .unwrap_err()
            .errors
            .iter()
            .any(|error| error.fix.contains("requires identity_file"))
    );
    let registry = root.0.join("identities.json");
    fs::write(&registry, r#"{"identities":[{"id":"local-admin","tokens":["test-token"],"project_roles":{"test-project":["roles/storage.admin"]}}]}"#).unwrap();
    args.extend(["--identity-file".into(), registry.to_str().unwrap().into()]);
    let config = Config::load(&args, &BTreeMap::new()).unwrap();
    assert_eq!(config.auth_mode, gcs_mimic::auth::AuthMode::Strict);
    assert_eq!(config.sync_dir, Some(root.0.join("tree")));
    assert_eq!(config.identity_file, Some(registry));
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn sync_dir_is_rejected_where_synchronization_is_unsupported() {
    let root = TestDir::new();
    let errors = Config::load(&settings(&root), &BTreeMap::new())
        .unwrap_err()
        .errors;
    assert!(
        errors
            .iter()
            .any(|error| error.key == "--sync-dir" && error.fix.contains("Linux and macOS only")),
        "{errors:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn sync_settings_precedence_defaults_boolean_and_startup_only_reinitialization() {
    let root = TestDir::new();
    let mut args = settings(&root);
    let config = Config::load(&args, &BTreeMap::new()).unwrap();
    assert!(config.sync_delete_guard);
    assert_eq!(config.sync_dir, Some(root.0.join("tree")));
    let file = root.0.join("config.json");
    for dir in ["file-tree", "env-tree"] {
        fs::create_dir(root.0.join(dir)).unwrap();
    }
    fs::write(
        &file,
        serde_json::json!({"sync_dir": root.0.join("file-tree"), "sync_delete_guard":false})
            .to_string(),
    )
    .unwrap();
    let mut file_args = vec![
        "--config".into(),
        file.to_str().unwrap().into(),
        "--data-dir".into(),
        args[3].clone(),
    ];
    let config = Config::load(&file_args, &BTreeMap::new()).unwrap();
    assert_eq!(config.sync_dir, Some(root.0.join("file-tree")));
    assert!(!config.sync_delete_guard);
    let env = BTreeMap::from([
        (
            "GCS_MIMIC_SYNC_DIR".into(),
            root.0.join("env-tree").to_str().unwrap().into(),
        ),
        ("GCS_MIMIC_SYNC_DELETE_GUARD".into(), "true".into()),
    ]);
    let config = Config::load(&file_args, &env).unwrap();
    assert_eq!(config.sync_dir, Some(root.0.join("env-tree")));
    assert!(config.sync_delete_guard);
    file_args.extend([
        "--sync-dir".into(),
        args[1].clone(),
        "--sync-delete-guard".into(),
        "false".into(),
    ]);
    let config = Config::load(&file_args, &env).unwrap();
    assert_eq!(config.sync_dir, Some(root.0.join("tree")));
    assert!(!config.sync_delete_guard);
    args.extend(["--config".into(), file.to_str().unwrap().into()]);
    args.extend([
        "--sync-reinitialize".into(),
        "0123456789abcdef0123456789abcdef".into(),
    ]);
    assert!(
        Config::load(&args, &env)
            .unwrap()
            .sync_reinitialize
            .is_some()
    );
    assert!(
        Config::load(
            &settings(&root),
            &BTreeMap::from([(
                "GCS_MIMIC_SYNC_REINITIALIZE".into(),
                "0123456789abcdef0123456789abcdef".into()
            )])
        )
        .is_err()
    );
    fs::write(
        file,
        r#"{"sync_reinitialize":"0123456789abcdef0123456789abcdef"}"#,
    )
    .unwrap();
    assert!(Config::load(&args, &env).is_err());
}

#[test]
fn sync_removed_settings_and_guard_without_root_fail_with_diagnostics() {
    let root = TestDir::new();
    for name in [
        "sync_settle_ms",
        "sync_delete_min_objects",
        "sync_delete_percent",
        "sync_conflict_max_bytes",
        "sync_conflict_max_entries",
    ] {
        let flag = format!("--{}", name.replace('_', "-"));
        let env_key = format!("GCS_MIMIC_{}", name.to_ascii_uppercase());
        let mut args = settings(&root);
        args.extend([flag.clone(), "1000".into()]);
        let errors = Config::load(&args, &BTreeMap::new()).unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == flag && error.fix.starts_with("Unknown setting"))
        );
        let env = BTreeMap::from([(env_key.clone(), "1000".into())]);
        let errors = Config::load(&settings(&root), &env).unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == env_key && error.fix.starts_with("Unknown setting"))
        );
        let file = root.0.join("removed.json");
        fs::write(&file, serde_json::json!({name:1000}).to_string()).unwrap();
        let mut args = settings(&root);
        args.extend(["--config".into(), file.to_str().unwrap().into()]);
        let errors = Config::load(&args, &BTreeMap::new()).unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == name && error.fix.starts_with("Unknown setting"))
        );
    }
    for value in ["TRUE", "1", "", "yes"] {
        let mut args = settings(&root);
        args.extend(["--sync-delete-guard".into(), value.into()]);
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
        assert!(
            Config::load(
                &settings(&root),
                &BTreeMap::from([("GCS_MIMIC_SYNC_DELETE_GUARD".into(), value.into())])
            )
            .is_err()
        );
        let file = root.0.join("guard.json");
        fs::write(
            &file,
            serde_json::json!({"sync_delete_guard":value}).to_string(),
        )
        .unwrap();
        let mut args = settings(&root);
        args.extend(["--config".into(), file.to_str().unwrap().into()]);
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
    }
    for value in ["true", "false"] {
        let errors = Config::load(
            &["--sync-delete-guard".into(), value.into()],
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == "--sync-delete-guard"
                    && error.fix.contains("requires sync_dir"))
        );
    }
    let errors = Config::load(
        &[],
        &BTreeMap::from([("GCS_MIMIC_SYNC_DELETE_GUARD".into(), "false".into())]),
    )
    .unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| error.key == "GCS_MIMIC_SYNC_DELETE_GUARD"
                && error.fix.contains("requires sync_dir"))
    );
    let file = root.0.join("guard.json");
    fs::write(&file, r#"{"sync_delete_guard":false}"#).unwrap();
    let errors = Config::load(
        &["--config".into(), file.to_str().unwrap().into()],
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert!(
        errors.errors.iter().any(
            |error| error.key == "sync_delete_guard" && error.fix.contains("requires sync_dir")
        )
    );
}

#[test]
fn sync_overlaps_aliases_empty_and_symlink_roots_are_rejected() {
    let root = TestDir::new();
    let mut args = settings(&root);
    args[3] = root.0.join("tree/data").to_str().unwrap().into();
    assert!(Config::load(&args, &BTreeMap::new()).is_err());
    args = settings(&root);
    args.extend(["--seed-dir".into(), root.0.to_str().unwrap().into()]);
    assert!(Config::load(&args, &BTreeMap::new()).is_err());
    args = settings(&root);
    args[1] = String::new();
    assert!(Config::load(&args, &BTreeMap::new()).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.0.join("tree"), root.0.join("alias")).unwrap();
        args = settings(&root);
        args[1] = root.0.join("alias").to_str().unwrap().into();
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
        args = settings(&root);
        args[3] = root.0.join("alias").to_str().unwrap().into();
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
    }
}
