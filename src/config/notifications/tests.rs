use super::*;

#[test]
fn no_selected_source_events_disables_the_global_rule_without_enabling_all_events() {
    let mut config = Config {
        pubsub_emulator_host: Some("127.0.0.1:8085".into()),
        event_pubsub_project_id: "local".into(),
        event_pubsub_topic: "events".into(),
        event_list: "none".into(),
        ..Config::default()
    };
    let runtime = config.notification_runtime().unwrap();
    assert!(runtime.endpoint.is_some());
    assert!(runtime.global.is_none());
    config.event_list = "none,finalize".into();
    assert!(config.notification_runtime().is_err());
    config.event_list = "finalize,delete,metadataUpdate,archive".into();
    assert_eq!(
        config
            .notification_runtime()
            .unwrap()
            .global
            .unwrap()
            .notification
            .event_types
            .len(),
        4
    );
}

#[test]
fn legacy_events_and_emulator_host_apply_with_native_precedence() {
    let env = BTreeMap::from([
        (
            "FAKE_GCS_EVENT_PUBSUB_PROJECT_ID".into(),
            "local-project".into(),
        ),
        ("FAKE_GCS_EVENT_PUBSUB_TOPIC".into(), "uploads".into()),
        ("PUBSUB_EMULATOR_HOST".into(), "127.0.0.1:8085".into()),
        (
            "FAKE_GCS_EVENT_LIST".into(),
            "finalize, archive,delete,metadataUpdate".into(),
        ),
    ]);
    let args: Vec<String> = ["--event-bucket", "selected", "--event-object-prefix", "in/"]
        .map(str::to_owned)
        .to_vec();
    let config = Config::load(&args, &env).unwrap();
    let runtime = config.notification_runtime().unwrap();
    assert!(runtime.endpoint.is_some());
    let global = runtime.global.unwrap();
    assert_eq!(global.bucket, "selected");
    assert_eq!(global.notification.object_name_prefix, "in/");
    assert_eq!(global.notification.event_types.len(), 4);
    assert_eq!(
        global.notification.topic,
        "//pubsub.googleapis.com/projects/local-project/topics/uploads"
    );
}

#[test]
fn event_settings_never_fall_back_to_production_or_ignore_incomplete_filters() {
    for pairs in [
        vec![("--event-pubsub-topic", "uploads")],
        vec![("--event-bucket", "selected")],
        vec![("--event-list", "invalid")],
        vec![("--pubsub-emulator-host", "pubsub.googleapis.com:80")],
        vec![
            ("--event-pubsub-project-id", "local"),
            ("--event-pubsub-topic", "uploads"),
        ],
    ] {
        let args: Vec<String> = pairs
            .into_iter()
            .flat_map(|(key, value)| [key.into(), value.into()])
            .collect();
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
    }
}

#[test]
fn native_emulator_host_wins_over_standard_alias_in_both_environment_readers() {
    let env = BTreeMap::from([
        ("PUBSUB_EMULATOR_HOST".into(), "127.0.0.1:8085".into()),
        (
            "GCS_MIMIC_PUBSUB_EMULATOR_HOST".into(),
            "127.0.0.1:8086".into(),
        ),
    ]);
    let config = Config::load(&[], &env).unwrap();
    assert_eq!(
        config.pubsub_emulator_host.as_deref(),
        Some("127.0.0.1:8086")
    );
    for entries in [
        vec![
            ("PUBSUB_EMULATOR_HOST", "127.0.0.1:8085"),
            ("GCS_MIMIC_PUBSUB_EMULATOR_HOST", "127.0.0.1:8086"),
        ],
        vec![
            ("GCS_MIMIC_PUBSUB_EMULATOR_HOST", "127.0.0.1:8086"),
            ("PUBSUB_EMULATOR_HOST", "127.0.0.1:8085"),
        ],
    ] {
        let report = Config::preflight_os(
            &[],
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        assert!(!report.diagnostics.is_err());
        assert_eq!(
            report.config.pubsub_emulator_host.as_deref(),
            Some("127.0.0.1:8086")
        );
    }
}
