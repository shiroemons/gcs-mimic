use crate::{
    auth::{BUCKETS_CREATE, TRUSTED_APP_ID, TestSigner, signature::SignedHostPort, test_signer},
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

/// A registry document, with an entry for the built-in signer's ID when `declared`.
fn registry(declared: bool) -> String {
    let mut identities = vec![admin()];
    if declared {
        identities.push(json!({"id": test_signer::ID, "tokens": []}));
    }
    json!({ "identities": identities }).to_string()
}

fn test_signer_warnings(report: &ConfigurationReport) -> Vec<&Diagnostic> {
    report
        .diagnostics
        .warnings
        .iter()
        .filter(|warning| {
            warning
                .fix
                .contains("test-signer@gcs-mimic.invalid is published")
        })
        .collect()
}

/// Strict mode enables the signer when the identity document declares its ID, unless
/// `test_signer` says otherwise, and warns at the setting that enabled it.
#[test]
fn strict_mode_settles_the_test_signer_from_the_identity_document() {
    let dir = TestDir::new();
    for declared in [false, true] {
        for (flag, value, _) in sources(&dir, &registry(declared)) {
            for (explicit, expected, warned_at) in [
                (None, declared, declared.then_some(flag)),
                (Some("enabled"), true, Some("--test-signer")),
                (Some("disabled"), false, None),
            ] {
                let mut args = vec!["--auth-mode", "strict", flag, value.as_str()];
                args.extend(
                    explicit
                        .into_iter()
                        .flat_map(|value| ["--test-signer", value]),
                );
                let mut report = report(&args);
                let auth = report.load_auth();
                assert!(
                    !report.diagnostics.is_err(),
                    "{args:?}: {}",
                    report.diagnostics
                );
                let expected = if expected {
                    TestSigner::Enabled
                } else {
                    TestSigner::Disabled
                };
                assert_eq!(auth.test_signer(), expected, "{args:?}");
                // A declared entry stays one declared identity; the signer adds none.
                assert_eq!(auth.identity_count(), Some(1 + usize::from(declared)));
                let warnings = test_signer_warnings(&report);
                match warned_at {
                    None => assert!(warnings.is_empty(), "{args:?}: {warnings:?}"),
                    Some(key) => {
                        assert_eq!(warnings.len(), 1, "{args:?}: {warnings:?}");
                        assert_eq!(warnings[0].key, key);
                        assert_eq!(warnings[0].source, DiagnosticSource::Cli);
                        let fix = &warnings[0].fix;
                        for expected in [
                            "anyone can authenticate",
                            "allAuthenticatedUsers",
                            "only for local tests",
                        ] {
                            assert!(fix.contains(expected), "{fix}");
                        }
                        // Only the implicit role is worth naming; a declared entry has its own.
                        let implicit = "It holds roles/storage.objectUser in local-project, so it can read and write objects there.";
                        assert_eq!(fix.contains(implicit), !declared, "{fix}");
                        if explicit.is_none() {
                            let setting = flag.trim_start_matches("--").replace('-', "_");
                            assert!(
                                fix.starts_with(&format!("{setting} declares {}", test_signer::ID)),
                                "{fix}"
                            );
                            assert!(fix.contains("test_signer=disabled"), "{fix}");
                        }
                    }
                }
            }
        }
    }
    // The warning names the input that enabled the signer, from whichever source it came.
    let env = BTreeMap::from([
        ("GCS_MIMIC_AUTH_MODE".to_owned(), "strict".to_owned()),
        ("GCS_MIMIC_IDENTITIES".to_owned(), registry(true)),
    ]);
    let mut report = Config::preflight(&[], &env);
    assert_eq!(report.load_auth().test_signer(), TestSigner::Enabled);
    let warnings = test_signer_warnings(&report);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        (warnings[0].source, warnings[0].key.as_str()),
        (DiagnosticSource::Environment, "GCS_MIMIC_IDENTITIES")
    );
}

/// Permissive defaults are unchanged: the signer exists exactly where signatures verify, and a
/// declared entry changes nothing there.
#[test]
fn permissive_test_signer_defaults_ignore_the_identity_document() {
    let dir = TestDir::new();
    let public = include_str!("../../../fixtures/signing/test-only-public.pem");
    let document = json!({"identities":[{"id": test_signer::ID, "public_keys": [public]}]});
    let path = dir.0.join("keys.json");
    std::fs::write(&path, document.to_string()).unwrap();
    let path = path.to_str().unwrap();
    for (args, expected) in [
        (&[][..], TestSigner::Disabled),
        (&["--signed-urls", "verify"], TestSigner::Enabled),
        (
            &["--signed-urls", "verify", "--identity-file", path],
            TestSigner::Enabled,
        ),
        (
            &["--signed-urls", "verify", "--test-signer", "disabled"],
            TestSigner::Disabled,
        ),
        (
            &[
                "--signed-urls",
                "verify",
                "--identity-file",
                path,
                "--test-signer",
                "disabled",
            ],
            TestSigner::Disabled,
        ),
    ] {
        let mut report = report(args);
        let auth = report.load_auth();
        assert!(
            !report.diagnostics.is_err(),
            "{args:?}: {}",
            report.diagnostics
        );
        assert_eq!(auth.test_signer(), expected, "{args:?}");
        assert!(report.diagnostics.warnings.is_empty(), "{args:?}");
    }
}

/// The published key marks a local test, so the host port is lenient wherever the built-in
/// signer is enabled; an explicit value wins, and an explicit lenient where nothing verifies
/// still fails validation.
#[test]
fn the_host_port_is_lenient_by_default_where_the_test_signer_is_enabled() {
    let dir = TestDir::new();
    let [declared, undeclared] = [true, false].map(|declared| {
        let path = dir.0.join(format!("identities-{declared}.json"));
        std::fs::write(&path, registry(declared)).unwrap();
        path.to_str().unwrap().to_owned()
    });
    fn strict(path: &str) -> Vec<&str> {
        vec!["--auth-mode", "strict", "--identity-file", path]
    }
    for (base, extra, test_signer, signed_host_port) in [
        (
            strict(&declared),
            &[][..],
            TestSigner::Enabled,
            SignedHostPort::Lenient,
        ),
        (
            strict(&declared),
            &["--signed-host-port", "exact"],
            TestSigner::Enabled,
            SignedHostPort::Exact,
        ),
        (
            strict(&undeclared),
            &["--test-signer", "enabled"],
            TestSigner::Enabled,
            SignedHostPort::Lenient,
        ),
        (
            strict(&undeclared),
            &[],
            TestSigner::Disabled,
            SignedHostPort::Exact,
        ),
        (
            strict(&undeclared),
            &["--signed-host-port", "lenient"],
            TestSigner::Disabled,
            SignedHostPort::Lenient,
        ),
        (
            vec!["--signed-urls", "verify"],
            &[],
            TestSigner::Enabled,
            SignedHostPort::Lenient,
        ),
        (
            vec!["--signed-urls", "verify"],
            &["--test-signer", "disabled"],
            TestSigner::Disabled,
            SignedHostPort::Exact,
        ),
        (vec![], &[], TestSigner::Disabled, SignedHostPort::Exact),
        (
            vec![],
            &["--signed-host-port", "exact"],
            TestSigner::Disabled,
            SignedHostPort::Exact,
        ),
    ] {
        let args = [base, extra.to_vec()].concat();
        let mut report = report(&args);
        let auth = report.load_auth();
        assert!(
            !report.diagnostics.is_err(),
            "{args:?}: {}",
            report.diagnostics
        );
        assert_eq!(auth.test_signer(), test_signer, "{args:?}");
        assert_eq!(auth.signed_host_port(), signed_host_port, "{args:?}");
    }
    let errors = Config::preflight(
        &strings(&["--signed-host-port", "lenient"]),
        &BTreeMap::new(),
    )
    .diagnostics
    .errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0]
            .fix
            .contains("has no effect while signatures are ignored")
    );
}

/// Without `trusted_principal` the trusted listener acts as the built-in application identity,
/// which manages the projects the setup names: the buckets document's and the seed project.
#[test]
fn the_trusted_listener_acts_as_the_built_in_app_in_the_setups_projects() {
    let dir = TestDir::new();
    let buckets = json!({"version":1,"project":"project-a","buckets":[
        {"name":"a-bucket"},{"name":"b-bucket","project":"project-b"}]})
    .to_string();
    let buckets_file = dir.0.join("buckets.json");
    std::fs::write(&buckets_file, &buckets).unwrap();
    let app = |auth: &crate::auth::Auth| {
        auth.principals()
            .unwrap()
            .iter()
            .find(|identity| identity.id() == TRUSTED_APP_ID)
            .cloned()
    };
    for (flag, value, _) in sources(&dir, &registry(false)) {
        for bucket_args in [
            ["--buckets", buckets.as_str()],
            ["--buckets-file", buckets_file.to_str().unwrap()],
        ] {
            let mut args = vec!["--auth-mode", "strict", flag, value.as_str()];
            args.extend(["--trusted-listen", "127.0.0.1:0"]);
            args.extend(bucket_args);
            let mut report = report(&args);
            let auth = report.load_auth();
            assert!(
                !report.diagnostics.is_err(),
                "{args:?}: {}",
                report.diagnostics
            );
            assert_eq!(auth.trusted_principal(), Some(TRUSTED_APP_ID));
            // The built-in identity is not one the document declares.
            assert_eq!(auth.identity_count(), Some(1));
            let app = app(&auth).unwrap();
            for project in ["local-project", "project-a", "project-b"] {
                assert!(app.allows(BUCKETS_CREATE, project), "{project}");
            }
            assert!(!app.allows(BUCKETS_CREATE, "other-project"));
        }
    }
    // Without a buckets document the seed project is all the setup names.
    let path = dir.0.join("identities.json");
    let path = path.to_str().unwrap();
    let mut seeded = report(&[
        "--auth-mode",
        "strict",
        "--identity-file",
        path,
        "--trusted-listen",
        "127.0.0.1:0",
        "--seed-project",
        "seeded-project",
    ]);
    let auth = seeded.load_auth();
    let seeded_app = app(&auth).unwrap();
    assert!(seeded_app.allows(BUCKETS_CREATE, "seeded-project"));
    assert!(!seeded_app.allows(BUCKETS_CREATE, "local-project"));
    // A named principal is used instead, and the built-in identity does not exist.
    let mut named = report(&[
        "--auth-mode",
        "strict",
        "--identity-file",
        path,
        "--trusted-listen",
        "127.0.0.1:0",
        "--trusted-principal",
        "admin@example.invalid",
    ]);
    let auth = named.load_auth();
    assert!(!named.diagnostics.is_err(), "{}", named.diagnostics);
    assert_eq!(auth.trusted_principal(), Some("admin@example.invalid"));
    assert!(app(&auth).is_none());
}

/// A declared entry for the application identity replaces the implicit grants.
#[test]
fn a_declared_app_entry_replaces_the_implicit_grants() {
    let dir = TestDir::new();
    let document = json!({"identities":[admin(), {"id": TRUSTED_APP_ID, "tokens": ["app-token"],
        "project_roles": {"local-project": ["roles/storage.objectViewer"]}}]})
    .to_string();
    for (flag, value, _) in sources(&dir, &document) {
        let mut report = report(&[
            "--auth-mode",
            "strict",
            flag,
            &value,
            "--trusted-listen",
            "127.0.0.1:0",
        ]);
        let auth = report.load_auth();
        assert_eq!(auth.trusted_principal(), Some(TRUSTED_APP_ID));
        assert_eq!(auth.identity_count(), Some(2));
        let app = auth
            .principals()
            .unwrap()
            .iter()
            .find(|identity| identity.id() == TRUSTED_APP_ID)
            .unwrap();
        assert!(app.allows(crate::auth::OBJECTS_GET, "local-project"));
        assert!(!app.allows(BUCKETS_CREATE, "local-project"));
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
