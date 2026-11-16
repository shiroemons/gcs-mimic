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
        "--auth-mode",
        "strict",
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
                        Setting::CorsOrigin("http://localhost:3000".parse().unwrap()),
                        Setting::CorsMethod("GET".parse().unwrap()),
                        Setting::CorsMethod("PUT".parse().unwrap()),
                        Setting::PublicAccessPrevention(PublicAccessPrevention::Inherited),
                        Setting::PublicRead,
                    ],
                ),
                expected(
                    "app-exports",
                    [Setting::IamBinding {
                        role: "roles/storage.objectAdmin".into(),
                        members: "user:a@example.com,user:b@example.com".parse().unwrap(),
                    }],
                ),
            ],
            format: Some(Format::Compose),
            auth_mode: Some(AuthMode::Strict),
            output: Some(PathBuf::from("buckets.yaml")),
        }
    );
}

/// Without options nothing is chosen yet, so the questions ask for the format and who reads.
#[test]
fn without_options_no_bucket_or_format_is_chosen() {
    let request = parse(&[]).unwrap();
    assert!(request.buckets.is_empty());
    assert_eq!(request.format, None);
    assert_eq!(request.auth_mode, None);
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
            &["--auth-mode", "private"],
            "--auth-mode",
            "permissive or strict",
        ),
        (
            &["--auth-mode", "strict", "--auth-mode", "strict"],
            "--auth-mode",
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
            &["--name", "app-uploads", "--cors-origin", "http://a b"],
            "--cors-origin",
            "visible ASCII",
        ),
        (
            &["--name", "app-uploads", "--cors-method", "GE T"],
            "--cors-method",
            "HTTP method",
        ),
        (
            &[
                "--name",
                "app-uploads",
                "--cors-response-header",
                "Content Type",
            ],
            "--cors-response-header",
            "header name",
        ),
        (
            &[
                "--name",
                "app-uploads",
                "--iam-binding",
                "roles/storage.objectViewer=dev@example.com",
            ],
            "--iam-binding",
            "Invalid member",
        ),
        (
            &[
                "--name",
                "app-uploads",
                "--iam-binding",
                "roles/storage.objectViewer=group:dev@example.com",
            ],
            "--iam-binding",
            "unsupported",
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

/// The diagnostic a parsed command line gets when its document is refused.
fn refusal(values: &[&str]) -> (String, String) {
    let request = parse(&args(values)).unwrap();
    let diagnostics = Document::new(request.buckets)
        .render(Format::Json)
        .map_err(rejected)
        .unwrap_err();
    let diagnostic = &diagnostics.errors[0];
    (diagnostic.key.clone(), diagnostic.fix.clone())
}

/// A CORS rule that matches no request names the option that completes it, with an example to
/// add after that bucket's --name.
#[test]
fn an_incomplete_cors_rule_names_the_option_that_completes_it() {
    let origin = ["--cors-origin", "http://localhost:3000"];
    let method = ["--cors-method", "PUT"];
    let header = ["--cors-response-header", "Content-Type"];
    for (settings, key, says) in [
        (
            origin.as_slice(),
            "--cors-method",
            "no --cors-method, so its rule matches no request; add one after --name app-uploads, for example --cors-method GET",
        ),
        (
            &method,
            "--cors-origin",
            "for example --cors-origin http://localhost:3000.",
        ),
        (
            &header,
            "--cors-origin",
            "no --cors-origin or --cors-method",
        ),
    ] {
        let mut values = vec!["--name", "app-exports", "--name", "app-uploads"];
        values.extend_from_slice(settings);
        let (actual_key, actual_fix) = refusal(&values);
        assert_eq!(actual_key, key, "{values:?}");
        assert!(
            actual_fix.starts_with("app-uploads has CORS settings"),
            "{actual_fix}"
        );
        assert!(actual_fix.contains(says), "{values:?}: {actual_fix}");
    }
}

fn advised(values: &[&str], format: Format, destination: Destination<'_>) -> String {
    let request = parse(&args(values)).unwrap();
    let auth_mode = request.auth_mode.unwrap_or(AuthMode::Permissive);
    advice(
        &Document::new(request.buckets).with_auth_mode(auth_mode),
        format,
        destination,
    )
}

/// The advice names the next command for where the document went, and a path is quoted only
/// when a shell would split or expand it.
#[test]
fn advice_names_the_next_step_for_the_format_and_destination() {
    let named = ["--name", "app-uploads"];
    for (path, word) in [
        ("buckets.json", "buckets.json"),
        ("config/buckets.json", "config/buckets.json"),
        ("my buckets.json", "'my buckets.json'"),
        ("it's.json", r"'it'\''s.json'"),
        ("$HOME.json", "'$HOME.json'"),
    ] {
        assert_eq!(
            advised(&named, Format::Json, Destination::File(Path::new(path))),
            format!("Next: gcs-mimic serve --buckets-file {word}\n")
        );
    }
    for (format, destination, says) in [
        (
            Format::Json,
            Destination::Terminal,
            "save this as buckets.json",
        ),
        (
            Format::Json,
            Destination::Redirected,
            "--buckets-file with the file this went to",
        ),
        (Format::Env, Destination::Terminal, "env_file"),
        (
            Format::Compose,
            Destination::Redirected,
            "docker compose up -d",
        ),
    ] {
        let advice = advised(&named, format, destination);
        assert!(advice.starts_with("Next: "), "{advice}");
        assert!(advice.contains(says), "{advice}");
        // One line within a common terminal's width: a wrapped paragraph goes unread.
        assert_eq!(advice.lines().count(), 1, "{advice}");
        assert!(advice.trim_end().len() <= 120, "{advice}");
    }
}

/// Strict mode moves the application's servers to the trusted listener, so its advice says where
/// servers and browsers connect, each in a line that fits a terminal, and a buckets file's serve
/// command carries strict mode's options.
#[test]
fn strict_advice_says_where_servers_and_browsers_connect() {
    let strict = ["--name", "app-private", "--auth-mode", "strict"];
    let file = Destination::File(Path::new("buckets.json"));
    let served = advised(&strict, Format::Json, file);
    assert!(
        served.starts_with(&format!(
            "Next: gcs-mimic serve --buckets-file buckets.json \\\n  {STRICT_SERVE_OPTIONS}\n"
        )),
        "{served}"
    );
    for (format, destination, servers) in [
        (Format::Json, file, "http://127.0.0.1:4444"),
        (Format::Json, Destination::Terminal, "http://127.0.0.1:4444"),
        (
            Format::Json,
            Destination::Redirected,
            "http://127.0.0.1:4444",
        ),
        (
            Format::Env,
            Destination::Terminal,
            "With docker run, choose Docker Compose",
        ),
        (
            Format::Compose,
            Destination::Redirected,
            "http://gcs:4444; never publish it",
        ),
    ] {
        let advice = advised(&strict, format, destination);
        assert!(advice.starts_with("Next: "), "{advice}");
        assert!(advice.contains(servers), "{advice}");
        assert!(
            advice.contains("public buckets are open there, private ones need signed URLs"),
            "{advice}"
        );
        // The key's command runs where the server does: in Compose, as its service.
        let credentials = match format {
            Format::Json | Format::Env => "the key that gcs-mimic test-signer credentials prints",
            Format::Compose => {
                "the key that docker compose run --rm gcs test-signer credentials prints"
            }
        };
        assert!(advice.contains(credentials), "{advice}");
        if format == Format::Json {
            assert!(advice.contains(STRICT_SERVE_OPTIONS), "{advice}");
        }
        assert!(advice.lines().all(|line| line.len() <= 120), "{advice}");
    }
}

/// Startup accepts an upload rule without Content-Type, so the advice warns about it instead.
#[test]
fn advice_notes_each_upload_rule_without_content_type() {
    let advice = advised(
        &[
            "--name",
            "app-uploads",
            "--cors-origin",
            "http://localhost:3000",
            "--cors-method",
            "PUT",
            "--name",
            "app-assets",
            "--cors-origin",
            "*",
            "--cors-method",
            "GET",
            "--name",
            "app-forms",
            "--cors-origin",
            "http://localhost:3000",
            "--cors-method",
            "POST",
            "--cors-response-header",
            "Content-Type",
        ],
        Format::Compose,
        Destination::Terminal,
    );
    let notes: Vec<_> = advice.lines().skip(1).collect();
    assert_eq!(
        notes,
        [
            "Note: browser uploads to app-uploads send Content-Type, which its CORS rule refuses; add --cors-response-header Content-Type"
        ]
    );
}

/// The goal flags add what the questions' goals add, so a script says what a bucket is for in
/// the words the questions use, and still needs an origin.
#[test]
fn goal_flags_add_what_the_goals_add() {
    let request = parse(&args(&[
        "--name",
        "app-uploads",
        "--browser-uploads",
        "--browser-downloads",
        "--cors-origin",
        "http://localhost:3000",
    ]))
    .unwrap();
    assert_eq!(
        request.buckets,
        [expected(
            "app-uploads",
            [
                Setting::BrowserUploads,
                Setting::BrowserDownloads,
                Setting::CorsOrigin("http://localhost:3000".parse().unwrap()),
            ],
        )]
    );
    let (key, fix) = refusal(&["--name", "app-assets", "--browser-downloads"]);
    assert_eq!(key, "--cors-origin");
    assert!(
        fix.contains("for example --cors-origin http://localhost:3000"),
        "{fix}"
    );
}

/// A saved document never replaces a file; a file that cannot be created keeps its cause.
#[test]
fn a_saved_document_never_replaces_a_file() {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("buckets.json");
    save_new(&path, "{}\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    assert!(matches!(save_new(&path, "other"), Err(SaveError::Exists)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    let missing = dir.0.join("missing").join("buckets.json");
    let error = save_new(&missing, "{}\n").unwrap_err();
    assert!(matches!(error, SaveError::Create(_)), "{error:?}");
    assert!(std::error::Error::source(&error).is_some());
    assert!(
        error.to_string().starts_with("Cannot create that file: "),
        "{error}"
    );
    assert!(!missing.exists());
}
