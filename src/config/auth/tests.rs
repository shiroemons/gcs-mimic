use crate::{
    config::{Config, ConfigurationReport},
    diagnostics::{Diagnostic, DiagnosticSource},
    test_support::TestDir,
};
use serde_json::json;
use std::collections::BTreeMap;

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn report(args: &[&str]) -> ConfigurationReport {
    let report = Config::preflight(&strings(args), &BTreeMap::new());
    assert!(
        !report.diagnostics.is_err(),
        "{args:?}: {}",
        report.diagnostics
    );
    report
}

fn admin() -> serde_json::Value {
    json!({"id":"admin@example.invalid","tokens":["secret-token"],
        "project_roles":{"local-project":["roles/storage.admin"]}})
}

/// The identity document from each setting: its flag, its value and the setting it names.
fn sources(dir: &TestDir, document: &str) -> [(&'static str, String, &'static str); 2] {
    let path = dir.0.join("identities.json");
    std::fs::write(&path, document).unwrap();
    [
        (
            "--identity-file",
            path.to_str().unwrap().to_owned(),
            "identity_file",
        ),
        ("--identities", document.to_owned(), "identities"),
    ]
}

fn identity_errors(report: &ConfigurationReport) -> Vec<&Diagnostic> {
    report
        .diagnostics
        .errors
        .iter()
        .filter(|error| error.source == DiagnosticSource::Identity)
        .collect()
}

#[test]
fn both_forms_load_the_same_registry() {
    let dir = TestDir::new();
    let document = json!({"identities": [admin()]}).to_string();
    for (flag, value, _) in sources(&dir, &document) {
        let mut report = report(&["--auth-mode", "strict", flag, &value]);
        let auth = report.load_auth();
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(auth.identity_count(), Some(1), "{flag}");
        assert!(auth.principals().unwrap()[0].allows(crate::auth::BUCKETS_CREATE, "local-project"));
    }
}

/// A failure names the setting that supplied the document and the entry inside it, and never
/// the document's text.
#[test]
fn load_failures_name_the_setting_and_the_entry_without_echoing_tokens() {
    let dir = TestDir::new();
    for (document, entry) in [
        (
            json!({"identities":[admin(),{"id":"other@example.invalid","tokens":["secret-token"]}]}).to_string(),
            "identities[1]: duplicate token",
        ),
        (
            json!({"identities":[{"id":"x","tokens":["secret-token"],"roles":[]}]}).to_string(),
            "identities[0]: unknown field: roles",
        ),
        ("{\"identities\":[".to_owned(), ""),
    ] {
        for (flag, value, setting) in sources(&dir, &document) {
            let mut report = report(&["--auth-mode", "strict", flag, &value]);
            report.load_auth();
            let errors = identity_errors(&report);
            assert_eq!(errors.len(), 1, "{setting}: {errors:?}");
            assert_eq!(errors[0].key, setting);
            assert!(
                errors[0].fix.starts_with(&format!("{setting}: {entry}")),
                "{errors:?}"
            );
            assert!(!errors[0].fix.contains("secret-token"), "{errors:?}");
        }
    }
}

#[test]
fn unknown_principals_name_the_setting_that_supplied_the_registry() {
    let dir = TestDir::new();
    let document = json!({"identities": [admin()]}).to_string();
    for (flag, value, setting) in sources(&dir, &document) {
        let mut report = report(&[
            "--auth-mode",
            "strict",
            flag,
            &value,
            "--admin-enabled",
            "true",
            "--admin-principal",
            "missing-admin@example.invalid",
            "--trusted-listen",
            "127.0.0.1:0",
            "--trusted-principal",
            "missing-app@example.invalid",
        ]);
        report.load_auth();
        let errors = identity_errors(&report);
        assert_eq!(errors.len(), 2, "{errors:?}");
        for (error, key) in errors.iter().zip(["admin_principal", "trusted_principal"]) {
            assert_eq!(error.key, key);
            assert!(
                error.fix.contains(&format!("is not an id in {setting};")),
                "{error:?}"
            );
        }
    }
}
