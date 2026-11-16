use super::*;
use gcs_mimic::bucket_init::generate::PublicAccessPrevention;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn fix(values: &[&str]) -> (String, String) {
    let diagnostics = parse(&args(values)).unwrap_err();
    let diagnostic = &diagnostics.errors[0];
    (diagnostic.key.clone(), diagnostic.fix.clone())
}

fn expected(name: &str, settings: impl IntoIterator<Item = Setting>) -> Bucket {
    let mut bucket = Bucket::new(name).unwrap();
    for setting in settings {
        bucket.apply(setting).unwrap();
    }
    bucket
}

/// Each setting belongs to the latest --name, and the global options may come anywhere.
#[test]
fn settings_attach_to_the_preceding_bucket() {
    let request = parse(&args(&[
        "--format",
        "compose",
        "--name",
        "app-uploads",
        "--versioning",
        "--label",
        "team=storage",
        "--cors-origin",
        "http://localhost:3000",
        "--cors-method",
        "GET",
        "--cors-method",
        "PUT",
        "--public-access-prevention",
        "inherited",
        "--public-read",
        "--name",
        "app-exports",
        "--iam-binding",
        "roles/storage.objectAdmin=user:a@example.com,user:b@example.com",
        "--output",
        "buckets.yaml",
    ]))
    .unwrap();
    assert_eq!(
        request,
        Request {
            buckets: vec![
                expected(
                    "app-uploads",
                    [
                        Setting::Versioning,
                        Setting::Label("team=storage".parse().unwrap()),
                        Setting::CorsOrigin("http://localhost:3000".into()),
                        Setting::CorsMethod("GET".into()),
                        Setting::CorsMethod("PUT".into()),
                        Setting::PublicAccessPrevention(PublicAccessPrevention::Inherited),
                        Setting::PublicRead,
                    ],
                ),
                expected(
                    "app-exports",
                    [Setting::IamBinding {
                        role: "roles/storage.objectAdmin".into(),
                        members: vec!["user:a@example.com".into(), "user:b@example.com".into()],
                    }],
                ),
            ],
            format: Some(Format::Compose),
            output: Some(PathBuf::from("buckets.yaml")),
        }
    );
}

/// Without options nothing is chosen yet, so the questions ask for the format too.
#[test]
fn without_options_no_bucket_or_format_is_chosen() {
    let request = parse(&[]).unwrap();
    assert!(request.buckets.is_empty());
    assert_eq!(request.format, None);
    assert_eq!(request.output, None);
}

#[test]
fn refused_arguments_name_their_option_and_the_remedy() {
    for (values, key, says) in [
        (
            ["--versioning"].as_slice(),
            "--versioning",
            "put --name NAME before it",
        ),
        (&["--name"], "--name", "needs a value"),
        (&["--name", "Bad-Name"], "--name", "lower-case letters"),
        (
            &["--name", "app-uploads", "--color", "red"],
            "--color",
            "Unknown option",
        ),
        (
            &[
                "--name",
                "app-uploads",
                "--location",
                "EU",
                "--location",
                "US",
            ],
            "--location",
            "location was already given",
        ),
        (&["--format", "yaml"], "--format", "json, env or compose"),
        (
            &["--format", "env", "--format", "json"],
            "--format",
            "given twice",
        ),
        (
            &["--name", "app-uploads", "--label", "team"],
            "--label",
            "key=value",
        ),
        (
            &["--name", "app-uploads", "--label", "Team=storage"],
            "--label",
            "lower-case letters",
        ),
        (
            &["--name", "app-uploads", "--iam-binding", "allUsers"],
            "--iam-binding",
            "ROLE=MEMBER",
        ),
        (
            &["--name", "app-uploads", "--cors-max-age-seconds", "1h"],
            "--cors-max-age-seconds",
            "whole seconds",
        ),
        (
            &["--name", "app-uploads", "--public-access-prevention", "on"],
            "--public-access-prevention",
            "inherited or enforced",
        ),
    ] {
        let (actual_key, actual_fix) = fix(values);
        assert_eq!(actual_key, key, "{values:?}");
        assert!(actual_fix.contains(says), "{values:?}: {actual_fix}");
    }
}
