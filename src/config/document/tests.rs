use super::*;
use crate::{
    config::{AuthProfile, BucketsMode, read_public_base_url, test_support::assert_precedence},
    diagnostics::DiagnosticSource,
    test_support::TestDir,
};
use std::collections::BTreeMap;

const IDENTITIES: &str =
    r#"{"identities":[{"id":"admin@example.invalid","tokens":["secret-token"]}]}"#;
const BUCKETS: &str = r#"{"version":1,"buckets":[{"name":"inline-bucket"}]}"#;

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn inline(text: &str) -> Option<DocumentSource> {
    Some(DocumentSource::Inline(text.into()))
}

#[test]
fn inline_documents_follow_precedence_from_every_source() {
    let dir = TestDir::new();
    let file = dir.0.join("config.json");
    // Only strict mode reads an identity registry, so its config file selects that mode too.
    for (contents, flag, variable, field) in [
        (
            serde_json::json!({"auth_mode": "strict", "identities": "file-text"}),
            "--identities",
            "GCS_MIMIC_IDENTITIES",
            (|config: Config| config.identities) as fn(Config) -> Option<DocumentSource>,
        ),
        (
            serde_json::json!({"buckets": "file-text"}),
            "--buckets",
            "GCS_MIMIC_BUCKETS",
            |config| config.buckets,
        ),
    ] {
        std::fs::write(&file, contents.to_string()).unwrap();
        let args = strings(&["--config", file.to_str().unwrap()]);
        let env = BTreeMap::from([(variable.to_owned(), "env-text".to_owned())]);
        assert_precedence(
            &args,
            &env,
            &[flag, "cli-text"],
            [
                None,
                inline("file-text"),
                inline("env-text"),
                inline("cli-text"),
            ],
            Config::load,
            field,
        );
    }
}

/// A config file may hold the document itself or a string that contains it; the object keeps
/// its exact text, so the document's own checks still see repeated members.
#[test]
fn a_config_file_holds_the_document_as_an_object_or_a_string() {
    let dir = TestDir::new();
    let file = dir.0.join("config.json");
    let args = strings(&["--config", file.to_str().unwrap()]);
    let empty = BTreeMap::new();
    let repeated = r#"{"version":1,"buckets":[],"buckets":[{"name":"a"}]}"#;
    std::fs::write(
        &file,
        format!(r#"{{"buckets": {repeated}, "identities": {IDENTITIES}}}"#),
    )
    .unwrap();
    let config = Config::load(&args, &empty).unwrap_err();
    // Identities without strict mode are rejected; the documents themselves were read.
    assert!(
        config
            .to_string()
            .contains("identities requires auth_mode=strict")
    );
    std::fs::write(
        &file,
        format!(r#"{{"auth_mode":"strict","buckets": {repeated}, "identities": {IDENTITIES}}}"#),
    )
    .unwrap();
    let config = Config::load(&args, &empty).unwrap();
    assert_eq!(config.buckets, inline(repeated));
    assert_eq!(config.identities, inline(IDENTITIES));
    std::fs::write(
        &file,
        serde_json::json!({"auth_mode":"strict","buckets": BUCKETS, "identities": IDENTITIES})
            .to_string(),
    )
    .unwrap();
    let config = Config::load(&args, &empty).unwrap();
    assert_eq!(config.buckets, inline(BUCKETS));
    assert_eq!(config.identities, inline(IDENTITIES));
    // The URL helper reads the same file, so it accepts what the server accepts.
    assert_eq!(read_public_base_url(file.to_str().unwrap()).unwrap(), None);
    for value in ["1", "[]", "true", "null"] {
        for setting in ["identities", "buckets"] {
            std::fs::write(&file, format!(r#"{{"{setting}": {value}}}"#)).unwrap();
            let errors = Config::load(&args, &empty).unwrap_err().errors;
            assert_eq!(errors.len(), 1, "{setting}={value}: {errors:?}");
            assert_eq!(errors[0].source, DiagnosticSource::Config);
            assert_eq!(errors[0].key, setting);
            assert_eq!(errors[0].fix, MEMBER_FIX);
            let diagnostic = read_public_base_url(file.to_str().unwrap()).unwrap_err();
            assert_eq!(
                (diagnostic.key.as_str(), diagnostic.fix.as_str()),
                (setting, MEMBER_FIX)
            );
        }
    }
}

#[test]
fn an_empty_inline_document_is_rejected_from_every_source() {
    let dir = TestDir::new();
    let file = dir.0.join("config.json");
    for (setting, flag, variable) in [
        ("identities", "--identities", "GCS_MIMIC_IDENTITIES"),
        ("buckets", "--buckets", "GCS_MIMIC_BUCKETS"),
    ] {
        std::fs::write(&file, serde_json::json!({ setting: "" }).to_string()).unwrap();
        for (source, key, args, env) in [
            (
                DiagnosticSource::Cli,
                flag,
                strings(&[flag, ""]),
                BTreeMap::new(),
            ),
            (
                DiagnosticSource::Environment,
                variable,
                vec![],
                BTreeMap::from([(variable.to_owned(), String::new())]),
            ),
            (
                DiagnosticSource::Config,
                setting,
                strings(&["--config", file.to_str().unwrap()]),
                BTreeMap::new(),
            ),
        ] {
            let errors = Config::load(&args, &env).unwrap_err().errors;
            assert_eq!(errors.len(), 1, "{key}: {errors:?}");
            assert_eq!((errors[0].source, errors[0].key.as_str()), (source, key));
            assert!(
                errors[0]
                    .fix
                    .starts_with(&format!("{setting} must not be empty")),
                "{errors:?}"
            );
        }
    }
}

#[test]
fn naming_both_settings_of_a_document_fails_and_names_both() {
    let empty = BTreeMap::new();
    for (file, inline, document, extra) in [
        (
            "--identity-file",
            "--identities",
            IDENTITIES,
            &["--auth-mode", "strict"][..],
        ),
        ("--buckets-file", "--buckets", BUCKETS, &[][..]),
    ] {
        let mut args = strings(&[file, "document.json", inline, document]);
        args.extend(strings(extra));
        let report = Config::preflight(&args, &empty);
        let errors = &report.diagnostics.errors;
        // One conflict, without a cascade from the profile both settings feed.
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, inline);
        let [file_setting, inline_setting] =
            [file, inline].map(|flag| flag.trim_start_matches("--").replace('-', "_"));
        assert!(errors[0].fix.contains(&file_setting), "{errors:?}");
        assert!(errors[0].fix.contains(&inline_setting), "{errors:?}");
        assert!(!errors[0].fix.contains("secret-token"));
        assert!(!report.valid(&[&file_setting, &inline_setting]));
        if inline == "--identities" {
            assert_eq!(report.auth_profile(), None);
        }
    }
    // The settings may come from different sources; precedence never picks one of them.
    let env = BTreeMap::from([("GCS_MIMIC_BUCKETS".to_owned(), BUCKETS.to_owned())]);
    let errors = Config::load(&strings(&["--buckets-file", "b.json"]), &env)
        .unwrap_err()
        .errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key, "GCS_MIMIC_BUCKETS");
}

/// Every pairing rule that names the identity file applies to the inline document too.
#[test]
fn inline_identities_pair_with_the_profile_like_an_identity_file() {
    let empty = BTreeMap::new();
    let profile = |args: &[&str]| Config::preflight(&strings(args), &empty);
    let report = profile(&["--auth-mode", "strict", "--identities", IDENTITIES]);
    assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
    assert!(matches!(
        report.auth_profile(),
        Some(AuthProfile::Strict {
            identities: Some(DocumentSource::Inline(_)),
            ..
        })
    ));
    let report = profile(&["--signed-urls", "verify", "--identities", IDENTITIES]);
    assert!(matches!(
        report.auth_profile(),
        Some(AuthProfile::VerifyingPermissive {
            identities: Some(DocumentSource::Inline(_)),
            ..
        })
    ));
    for args in [
        &["--identities", IDENTITIES][..],
        &["--identities", IDENTITIES, "--signed-urls", "ignore"],
    ] {
        let report = profile(args);
        let errors = &report.diagnostics.errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "--identities");
        assert!(
            errors[0]
                .fix
                .starts_with("identities requires auth_mode=strict, or signed_urls=verify"),
            "{errors:?}"
        );
        assert_eq!(report.auth_profile(), None);
    }
    let errors = profile(&["--auth-mode", "strict"]).diagnostics.errors;
    assert!(
        errors[0]
            .fix
            .contains("requires identity_file or identities"),
        "{errors:?}"
    );
    // Strict mode records the registered caller, whichever setting supplied the registry.
    let errors = profile(&[
        "--auth-mode",
        "strict",
        "--identities",
        IDENTITIES,
        "--local-creator-email",
        "writer@example.invalid",
    ])
    .diagnostics
    .errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key, "--local-creator-email");
}

#[test]
fn the_buckets_mode_accepts_either_document_setting() {
    let empty = BTreeMap::new();
    for args in [
        &["--buckets", BUCKETS, "--buckets-mode", "create"][..],
        &["--buckets-file", "b.json", "--buckets-mode", "create"],
    ] {
        assert!(Config::load(&strings(args), &empty).is_ok(), "{args:?}");
    }
    let errors = Config::load(&strings(&["--buckets-mode", "create"]), &empty)
        .unwrap_err()
        .errors;
    assert!(
        errors[0].fix.contains("requires buckets_file or buckets"),
        "{errors:?}"
    );
    // An invalid inline document is reported once, without a second error about the mode.
    let errors = Config::load(
        &strings(&["--buckets", "", "--buckets-mode", "reconcile"]),
        &empty,
    )
    .unwrap_err()
    .errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key, "--buckets");
}

/// The document is the source of truth wherever it comes from, so its edits apply by default
/// for a file as for an inline document. An explicit mode wins for both.
#[test]
fn the_buckets_mode_is_the_same_for_a_file_and_an_inline_document() {
    let empty = BTreeMap::new();
    for (args, form, expected) in [
        (
            &["--buckets", BUCKETS][..],
            Form::Inline,
            BucketsMode::Reconcile,
        ),
        (
            &["--buckets-file", "b.json"],
            Form::File,
            BucketsMode::Reconcile,
        ),
        (
            &["--buckets", BUCKETS, "--buckets-mode", "create"],
            Form::Inline,
            BucketsMode::Create,
        ),
        (
            &["--buckets-file", "b.json", "--buckets-mode", "create"],
            Form::File,
            BucketsMode::Create,
        ),
    ] {
        let config = Config::load(&strings(args), &empty).unwrap();
        let source = config.buckets.as_ref().unwrap();
        assert_eq!(source.form(), form, "{args:?}");
        assert_eq!(config.buckets_mode, expected, "{args:?}");
    }
}

/// Validation applies each setting to a copy of the configuration, so a copy must share the
/// document text instead of copying a buckets document of up to 1 GiB once per setting.
#[test]
fn a_configuration_copy_shares_its_inline_documents() {
    let config = Config::load(
        &strings(&[
            "--auth-mode",
            "strict",
            "--identities",
            IDENTITIES,
            "--buckets",
            BUCKETS,
        ]),
        &BTreeMap::new(),
    )
    .unwrap();
    let copy = config.clone();
    for (original, copy) in [
        (&config.identities, &copy.identities),
        (&config.buckets, &copy.buckets),
    ] {
        let (Some(DocumentSource::Inline(original)), Some(DocumentSource::Inline(copy))) =
            (original, copy)
        else {
            panic!("both documents are inline: {original:?} {copy:?}");
        };
        assert!(Arc::ptr_eq(original, copy));
    }
}

#[test]
fn debug_output_never_shows_inline_text() {
    let source = DocumentSource::Inline(IDENTITIES.into());
    let debug = format!("{source:?}");
    assert!(!debug.contains("secret-token"), "{debug}");
    assert!(debug.contains(&IDENTITIES.len().to_string()), "{debug}");
    let config = Config::load(
        &strings(&["--auth-mode", "strict", "--identities", IDENTITIES]),
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(!format!("{config:?}").contains("secret-token"));
}
