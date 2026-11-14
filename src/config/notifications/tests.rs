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
            .iter()
            .count(),
        4
    );
}

#[test]
fn alias_events_and_emulator_host_apply_with_native_precedence() {
    let env = BTreeMap::from([("PUBSUB_EMULATOR_HOST".into(), "127.0.0.1:8085".into())]);
    let args: Vec<String> = [
        "-event.pubsub-project-id",
        "local-project",
        "-event.pubsub-topic",
        "uploads",
        "-event.list",
        "finalize, archive,delete,metadataUpdate",
        "--event-bucket",
        "selected",
        "--event-object-prefix",
        "in/",
    ]
    .map(str::to_owned)
    .to_vec();
    let config = Config::load(&args, &env).unwrap();
    let runtime = config.notification_runtime().unwrap();
    assert!(runtime.endpoint.is_some());
    let global = runtime.global.unwrap();
    assert_eq!(global.bucket, "selected");
    assert_eq!(global.notification.object_name_prefix, "in/");
    assert_eq!(global.notification.event_types.iter().count(), 4);
    assert_eq!(
        global.notification.topic.as_str(),
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
fn event_setting_failures_name_their_setting_and_fix() {
    const COMPLETE: [(&str, &str); 3] = [
        ("--event-pubsub-project-id", "local"),
        ("--event-pubsub-topic", "uploads"),
        ("--pubsub-emulator-host", "127.0.0.1:8085"),
    ];
    let long_bucket = "b".repeat(223);
    let long_prefix = "p".repeat(1025);
    let complete_with = |setting: &'static str, value: &str| {
        let mut pairs: Vec<(&str, String)> = COMPLETE
            .iter()
            .map(|(key, value)| (*key, (*value).to_owned()))
            .collect();
        pairs.push((setting, value.to_owned()));
        pairs
    };
    // A failure the settings find together names the first given event setting in the order
    // `check_notifications` lists them, which is the emulator host whenever it is set.
    for (pairs, key, fix) in [
        (
            vec![(
                "--pubsub-emulator-host",
                "pubsub.googleapis.com:80".to_owned(),
            )],
            "--pubsub-emulator-host",
            "Pub/Sub requires an explicit local emulator host:port (HTTP, no credentials, path or query); public, metadata and Google endpoints are disabled",
        ),
        (
            vec![("--event-bucket", "selected".to_owned())],
            "--event-bucket",
            "Event filters require event_pubsub_project_id and event_pubsub_topic; remove the filters or configure a local Pub/Sub emulator",
        ),
        (
            vec![
                ("--event-pubsub-project-id", "local".to_owned()),
                ("--event-pubsub-topic", "uploads".to_owned()),
            ],
            "--event-pubsub-project-id",
            "Event publishing requires event_pubsub_project_id, event_pubsub_topic and an explicit pubsub_emulator_host (or PUBSUB_EMULATOR_HOST); production Google Pub/Sub is disabled",
        ),
        (
            vec![
                ("--event-pubsub-project-id", "local".to_owned()),
                ("--event-pubsub-topic", "bad topic".to_owned()),
                ("--pubsub-emulator-host", "127.0.0.1:8085".to_owned()),
            ],
            "--pubsub-emulator-host",
            "Invalid topic \"projects/local/topics/bad topic\"; use //pubsub.googleapis.com/projects/PROJECT/topics/TOPIC (for example, //pubsub.googleapis.com/projects/local-project/topics/uploads)",
        ),
        (
            complete_with("--event-bucket", &long_bucket),
            "--pubsub-emulator-host",
            "event_bucket exceeds the supported bucket name length",
        ),
        (
            complete_with("--event-object-prefix", &long_prefix),
            "--pubsub-emulator-host",
            "object_name_prefix exceeds 1024 bytes",
        ),
        (
            complete_with("--event-list", "finalize,invalid"),
            "--pubsub-emulator-host",
            "event_list requires finalize, delete, metadataUpdate or archive, separated by commas",
        ),
    ] {
        let args: Vec<String> = pairs
            .iter()
            .flat_map(|(key, value)| [(*key).to_owned(), value.clone()])
            .collect();
        let diagnostics = Config::load(&args, &BTreeMap::new()).unwrap_err();
        let errors: Vec<(&str, &str)> = diagnostics
            .errors
            .iter()
            .map(|error| (error.key.as_str(), error.fix.as_str()))
            .collect();
        assert_eq!(errors, [(key, fix)], "{pairs:?}");
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
