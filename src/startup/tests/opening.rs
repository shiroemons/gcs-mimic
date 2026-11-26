use super::*;
use gcs_mimic::storage::admin::{BucketDefinition, BucketName, Origin};
use std::{error::Error, process::Command};

const SCENARIO: &str = "GCSM_TEST_STARTUP_SIGNAL";

#[tokio::test]
async fn signals_preserve_startup_events_and_exit_status() {
    if let Ok(scenario) = std::env::var(SCENARIO) {
        let result = interrupt_open(&scenario).await;
        let code = crate::command_exit(result);
        std::process::exit(i32::from(code != std::process::ExitCode::SUCCESS));
    }
    let diagnostic = r#"{"event":"diagnostics","level":"error","diagnostics":[{"source":"catalog","key":"data_dir","fix":"Cannot reconcile stored bucket IAM and ACL state; inspect the catalog or restore a valid backup before restarting."}]}"#;
    for (scenario, code, stderr) in [
        ("before", 0, r#"{"event":"stopping","level":"info"}"#),
        ("recovery", 0, r#"{"event":"stopping","level":"info"}"#),
        ("reconciling", 0, r#"{"event":"stopping","level":"info"}"#),
        ("reconciliation-fails", 1, diagnostic),
        ("no-signal", 1, diagnostic),
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "startup::tests::opening::signals_preserve_startup_events_and_exit_status",
                "--nocapture",
            ])
            .env(SCENARIO, scenario)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "{scenario}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim_end(),
            stderr,
            "{scenario}"
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        // The child runs in libtest, which also writes its own banner to stdout.
        let events: Vec<_> = stdout
            .lines()
            .filter(|line| line.starts_with('{'))
            .collect();
        if code == 0 {
            assert_eq!(events, [r#"{"event":"stopped"}"#], "{scenario}: {stdout}");
        } else {
            assert!(events.is_empty(), "{scenario}: {stdout}");
        }
    }
}

async fn interrupt_open(scenario: &str) -> Result<(), crate::CliError> {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let bucket = BucketName::new("existing-bucket").unwrap();
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "project", &SystemClock).unwrap())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    if scenario != "reconciling" {
        rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap().execute_batch(
            "UPDATE buckets SET acl_policy_bound=0; INSERT OR REPLACE INTO acl_bucket_entries VALUES ('existing-bucket','bucket','[{}]');"
        ).unwrap();
    }
    let (entered, at_boundary) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::channel();
    let clock = Arc::new(RecoveryClock {
        skip_recovery: AtomicBool::new(matches!(scenario, "reconciling" | "reconciliation-fails")),
        entered: Mutex::new(Some(entered)),
        release: Mutex::new(released),
    });
    let (signal, signalled) = tokio::sync::oneshot::channel();
    let mut signal = Some(signal);
    if scenario == "before" {
        signal.take().unwrap().send(()).unwrap();
    }
    let shutdown = async { signalled.await.map_err(io::Error::other) };
    tokio::pin!(shutdown);
    let config = dir.config();
    let auth = Auth::permissive();
    let opening = open_catalog(&config, &auth, clock, shutdown.as_mut());
    tokio::pin!(opening);
    tokio::select! {
        _ = &mut opening => panic!("open finished before the clock barrier"),
        observed = at_boundary => observed.unwrap(),
    }
    if !matches!(scenario, "before" | "no-signal") {
        signal.take().unwrap().send(()).unwrap();
        tokio::select! {
            biased;
            _ = &mut opening => panic!("open must drain the blocked operation"),
            () = std::future::ready(()) => {}
        }
    }
    release.send(()).unwrap();
    let result = opening.await;
    let reopened = Catalog::open(&dir.0).await.unwrap();
    reopened.close().await.unwrap();
    assert!(matches!(result, Ok(PhaseResult::Stopped) | Err(_)));
    result.map(|_| ())
}

#[test]
fn authentication_diagnostics_preserve_the_catalog_and_io_causes() {
    let cli = open_failure(CatalogOpenError::Authentication(CatalogError::from(
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reconciliation read failed",
        ),
    )));
    let cause = cli
        .source()
        .unwrap()
        .downcast_ref::<CatalogError>()
        .unwrap();
    assert_eq!(
        cause
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let diagnostic = &cli.diagnostics().unwrap().errors[0];
    assert_eq!(diagnostic.source, DiagnosticSource::Catalog);
    assert_eq!(diagnostic.key, "data_dir");
    assert!(
        crate::CliError::Io(io::Error::other("x"))
            .diagnostics()
            .is_none()
    );
}
