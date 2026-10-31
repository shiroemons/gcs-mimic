//! Emulator-neutral probes for the official Rust Cloud Storage SDK
//! (`google-cloud-storage`); stdout is JSONL evidence, one line per case.

mod cases;
mod config;
mod harness;
mod transport;

use cases::{CASES, Case};
use config::{BoxError, Config};
use harness::Ctx;
use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

const CASE_TIMEOUT: Duration = Duration::from_secs(60);
const DETAIL_LIMIT: usize = 300;

fn main() -> ExitCode {
    // Must run before the runtime starts any thread; see `sanitize_process_env`.
    config::sanitize_process_env();
    let transport_check = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--transport-check" => true,
        _ => {
            eprintln!("usage: gcs-mimic-scorecard-rust [--transport-check]");
            return ExitCode::from(2);
        }
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(BoxError::from)
        .and_then(|runtime| runtime.block_on(run(transport_check)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("FAIL: {}", transport::chain(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

async fn run(transport_check: bool) -> Result<(), BoxError> {
    let config = Config::from_env()?;
    if transport_check {
        transport::check(&config).await?;
        println!("PASS: Rust SDK transport CA/SAN and signed-request exact-origin guard");
        return Ok(());
    }
    let ctx = Ctx::new(&config).await?;
    let mut stdout = std::io::stdout().lock();
    for (id, case) in CASES {
        if !id.contains(config.filter.as_str()) {
            continue;
        }
        let outcome = match case {
            Case::Unsupported(reason) => Outcome::Unsupported(reason),
            Case::Run(probe) => match tokio::time::timeout(CASE_TIMEOUT, probe(&ctx)).await {
                Ok(Ok(())) => Outcome::Pass,
                Ok(Err(error)) => Outcome::Fail(transport::chain(error.as_ref())),
                Err(_) => Outcome::Fail("case exceeded 60 seconds".into()),
            },
        };
        writeln!(stdout, "{}", outcome.line(id))?;
        stdout.flush()?;
    }
    Ok(())
}

enum Outcome {
    Pass,
    Fail(String),
    Unsupported(&'static str),
}

impl Outcome {
    fn line(&self, id: &str) -> String {
        let area = if id.starts_with("project.") {
            "bucket"
        } else {
            id.split('.').next().unwrap_or(id)
        };
        let head = format!(
            r#"{{"sdk":"rust","id":{},"area":{}"#,
            serde_json::Value::from(id),
            serde_json::Value::from(area)
        );
        let detail = |text: &str| {
            serde_json::Value::from(text.chars().take(DETAIL_LIMIT).collect::<String>())
        };
        match self {
            Self::Pass => format!(r#"{head},"pass":true}}"#),
            Self::Fail(error) => format!(r#"{head},"pass":false,"detail":{}}}"#, detail(error)),
            Self::Unsupported(reason) => {
                format!(
                    r#"{head},"pass":false,"unsupported":true,"detail":{}}}"#,
                    detail(reason)
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Outcome;

    #[test]
    fn lines_follow_the_scorecard_jsonl_shape() {
        assert_eq!(
            Outcome::Pass.line("project.hmac_keys"),
            r#"{"sdk":"rust","id":"project.hmac_keys","area":"bucket","pass":true}"#
        );
        let long = "x\"".repeat(400);
        let line = Outcome::Fail(long).line("object.move");
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        assert_eq!(parsed["area"], "object");
        assert_eq!(
            parsed["detail"].as_str().map(|d| d.chars().count()),
            Some(300)
        );
        let line = Outcome::Unsupported("none").line("signed.v2_get");
        assert!(line.ends_with(r#""pass":false,"unsupported":true,"detail":"none"}"#));
    }
}
