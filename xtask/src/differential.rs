//! Differential harness: sends the same declarative request sequences to the
//! real Cloud Storage service and to a local gcs-mimic, normalizes the
//! responses and reports every difference.
//!
//! The harness never contacts Google unless `GCS_MIMIC_DIFF_PROJECT` and
//! `GCS_MIMIC_DIFF_TOKEN` are both set. See `docs/en/development.md`.

mod base64;
mod bucket;
mod case;
mod diff;
mod guard;
mod normalize;
mod pace;
mod recording;
mod report;
mod rules;
mod runner;
mod server;
mod template;
mod transport;
mod xml;

use guard::Guarded;
use pace::Paced;
use runner::{CaseOutcome, Names, Side, run_side};
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use transport::{CurlTransport, GrpcTransport, Transport};

type Result<T> = std::result::Result<T, String>;

const DEFAULT_CASES: &str = "compatibility/differential/cases";
const DEFAULT_ENDPOINT: &str = "https://storage.googleapis.com";
const LOCAL_PROJECT: &str = "local-project";
/// The pinned googleapis protos the gcs-mimic build also uses (see `src/grpc/proto/SHA256SUMS`).
const PROTO_ROOT: &str = "src/grpc/proto";
const LIVE_GRPC_PORT: &str = "443";

pub const USAGE: &str = "Usage: cargo xtask differential validate [--cases DIR] | differential run [--cases DIR] [--filter TEXT] [--out DIR] [--record FILE] [--reference FILE] [--evidence FILE] [--binary PATH] [--fail-on-diff] | differential sweep";

#[derive(Default)]
struct Options {
    cases: Option<PathBuf>,
    filter: Option<String>,
    out: Option<PathBuf>,
    record: Option<PathBuf>,
    reference: Option<PathBuf>,
    evidence: Option<PathBuf>,
    binary: Option<PathBuf>,
    fail_on_diff: bool,
}

fn parse_options(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if flag == "--fail-on-diff" {
            options.fail_on_diff = true;
            continue;
        }
        let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let path = || PathBuf::from(value);
        match flag.as_str() {
            "--cases" => options.cases = Some(path()),
            "--filter" => options.filter = Some(value.clone()),
            "--out" => options.out = Some(path()),
            "--record" => options.record = Some(path()),
            "--reference" => options.reference = Some(path()),
            "--evidence" => options.evidence = Some(path()),
            "--binary" => options.binary = Some(path()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

fn random_hex() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 4];
    let filled = fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !filled {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        bytes = (nanos ^ std::process::id()).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// UTC calendar date from the system clock (civil-from-days).
fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

enum Mode {
    SelfCheck,
    Live {
        project: String,
        token: String,
        endpoint: String,
    },
    Replay(PathBuf),
}

fn select_mode(options: &Options) -> Result<Mode> {
    let project = env::var("GCS_MIMIC_DIFF_PROJECT")
        .ok()
        .filter(|v| !v.is_empty());
    let token = env::var("GCS_MIMIC_DIFF_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    match (project, token, &options.reference) {
        (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => Err(
            "--reference replays a recording; unset GCS_MIMIC_DIFF_PROJECT and GCS_MIMIC_DIFF_TOKEN".into(),
        ),
        (Some(project), Some(token), None) => {
            let endpoint = env::var("GCS_MIMIC_DIFF_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.into());
            if !endpoint.starts_with("https://") {
                return Err("GCS_MIMIC_DIFF_ENDPOINT must start with https://".into());
            }
            Ok(Mode::Live { project, token, endpoint })
        }
        (Some(_), None, None) | (None, Some(_), None) => Err(
            "set both GCS_MIMIC_DIFF_PROJECT and GCS_MIMIC_DIFF_TOKEN to contact the live service, or neither".into(),
        ),
        (None, None, Some(path)) => Ok(Mode::Replay(path.clone())),
        (None, None, None) => Ok(Mode::SelfCheck),
    }
}

pub fn validate(root: &Path, args: &[String]) -> Result<()> {
    let options = parse_options(args)?;
    let dir = root.join(options.cases.unwrap_or_else(|| DEFAULT_CASES.into()));
    let cases = case::load_cases(&dir)?;
    println!("{} differential cases are valid", cases.len());
    Ok(())
}

/// Writes `text` after redacting what the guard finds. When something was
/// still there the redacted file is kept for inspection and the error names
/// where it was (case, step, JSON path or header), never the value.
fn write_checked(path: &Path, text: &str, secrets: &[(String, &str)]) -> Result<()> {
    let leaks = report::find_leaks(text, secrets);
    let text = if leaks.is_empty() {
        text.to_owned()
    } else {
        report::redact_leaks(text, secrets)
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    if leaks.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}: redacted file written, but values were still present at:\n  {}",
        path.display(),
        leaks.join("\n  ")
    ))
}

/// Runs the harness. Returns whether the run counts as failed.
pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let options = parse_options(args)?;
    let mode = select_mode(&options)?;
    let dir = root.join(
        options
            .cases
            .clone()
            .unwrap_or_else(|| DEFAULT_CASES.into()),
    );
    let mut cases = case::load_cases(&dir)?;
    if let Some(filter) = &options.filter {
        cases.retain(|c| c.id.contains(filter.as_str()));
        if cases.is_empty() {
            return Err(format!("no case matches {filter:?}"));
        }
    }
    let replayed = match &mode {
        Mode::Replay(path) => Some(recording::from_json(&crate::read_json(path)?)?),
        _ => None,
    };
    let work = env::temp_dir().join(format!("gcsmimic-differential-{}", random_hex()));
    let scratch = work.join("scratch");
    fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let outcome = execute(root, &options, &mode, replayed, &cases, &work, &scratch);
    server::remove_dir(&scratch);
    server::remove_dir(&work.join("data"));
    outcome
}

/// What the live reference run produced.
struct LiveRun {
    run: runner::SideRun,
    requests: u64,
}

fn run_names() -> Names {
    let run_id = format!("{}{}", bucket::NAME_PREFIX, random_hex());
    Names {
        bucket: format!("{run_id}-b"),
        new_bucket: format!("{run_id}-n"),
        missing_bucket: format!("{run_id}-m"),
        own_bucket: format!("{run_id}-c"),
        run_id,
    }
}

/// The bucket names and endpoint the report must never contain.
fn base_secrets(names: &Names, endpoint: &str, case_count: usize) -> Vec<(String, &'static str)> {
    let mut secrets: Vec<(String, &str)> = vec![
        (names.bucket.clone(), "bucket name"),
        (names.missing_bucket.clone(), "bucket name"),
        (endpoint.to_owned(), "local endpoint"),
    ];
    secrets.extend((0..case_count).map(|index| (names.new_bucket_for(index), "bucket name")));
    secrets.extend(
        (0..case_count).map(|index| (format!("{}{index}", names.own_bucket), "bucket name")),
    );
    secrets
}

/// Runs the cases against the live service. Every request passes the guard
/// first and the pacing second, so a refused request costs no wait.
fn run_live(
    cases: &[case::Case],
    names: &Names,
    live: (&str, &str, &str),
    scratch: &Path,
    proto_root: &Path,
) -> LiveRun {
    let (project, token, endpoint) = live;
    let curl = CurlTransport::new(endpoint, Some(token.to_owned()), false, scratch);
    let paced = Paced::new(&curl);
    let guarded = Guarded { inner: &paced };
    let grpc = GrpcTransport::new(
        &format!(
            "{}:{LIVE_GRPC_PORT}",
            endpoint.trim_start_matches("https://")
        ),
        false,
        Some(token.to_owned()),
        proto_root,
    );
    let grpc_paced = Paced::new(&grpc);
    let grpc_guarded = Guarded { inner: &grpc_paced };
    let side = Side {
        transport: &guarded,
        grpc: Some(&grpc_guarded),
        project,
        hosts: live_hosts(endpoint),
        server_error_wait: Duration::from_secs(30),
    };
    println!(
        "running {} cases against the live reference service",
        cases.len()
    );
    let run = run_side(&side, cases, names);
    LiveRun {
        run,
        requests: curl.requests() + grpc.requests(),
    }
}

fn live_hosts(endpoint: &str) -> Vec<String> {
    vec![
        endpoint.to_owned(),
        endpoint.trim_start_matches("https://").to_owned(),
        "https://www.googleapis.com".into(),
        "www.googleapis.com".into(),
    ]
}

fn run_mimic(
    local: &server::LocalServer,
    cases: &[case::Case],
    names: &Names,
    scratch: &Path,
    proto_root: &Path,
) -> runner::SideRun {
    let curl = CurlTransport::new(&local.endpoint, None, true, scratch);
    let guarded = Guarded { inner: &curl };
    let grpc = local
        .grpc
        .as_deref()
        .map(|address| GrpcTransport::new(address, true, None, proto_root));
    let grpc_guarded = grpc.as_ref().map(|t| Guarded { inner: t });
    let side = Side {
        transport: &guarded,
        grpc: grpc_guarded.as_ref().map(|t| t as &dyn Transport),
        project: LOCAL_PROJECT,
        hosts: vec![
            local.endpoint.clone(),
            local.endpoint.trim_start_matches("http://").to_owned(),
        ],
        server_error_wait: Duration::from_secs(3),
    };
    println!(
        "running {} cases against gcs-mimic at the local endpoint",
        cases.len()
    );
    run_side(&side, cases, names)
}

/// The pinned storage.proto revision (`docs/en/sources.md`).
const PROTO_REVISION: &str = "d73dfb3c3be52245625b951b9c0bcf65163c549f";

/// What a recording must say about how its gRPC bodies were produced: the
/// proto, and grpcurl, which silently drops request fields the pinned proto
/// does not define and renders responses by its own rules.
fn tools_note() -> String {
    format!(
        "storage.proto {PROTO_REVISION}; {}; grpcurl silently drops request fields that are not in the pinned proto",
        transport::grpcurl_version()
    )
}

fn count_retries(cases: &[runner::CaseOutcome]) -> u32 {
    cases.iter().flat_map(|c| &c.steps).map(|s| s.retries).sum()
}

fn execute(
    root: &Path,
    options: &Options,
    mode: &Mode,
    replayed: Option<recording::Recording>,
    cases: &[case::Case],
    work: &Path,
    scratch: &Path,
) -> Result<bool> {
    let (runnable, skipped) = split_unrunnable(cases, transport::grpcurl_available());
    let cases = runnable.as_slice();
    let binary = match &options.binary {
        Some(path) => path.clone(),
        None => server::build(root)?,
    };
    let local = server::LocalServer::start(&binary, work)?;
    let names = run_names();
    let proto_root = root.join(PROTO_ROOT);
    let mimic_run = run_mimic(&local, cases, &names, scratch, &proto_root);
    let mut secrets = base_secrets(&names, &local.endpoint, cases.len());

    let mut reference_requests = None;
    let mut reference_retries = 0;
    let mut reference_cleanup = Vec::new();
    let mut reference_failed_cleanup = false;
    let mut generated = today();
    let reference_cases: Option<Vec<CaseOutcome>> = match mode {
        Mode::SelfCheck => None,
        Mode::Replay(_) => {
            let recorded = replayed.ok_or("recording was not loaded")?;
            generated = format!("{generated} (recording of {})", recorded.generated);
            reference_requests = Some(recorded.requests);
            reference_retries = count_retries(&recorded.cases);
            Some(recorded.cases)
        }
        Mode::Live {
            project,
            token,
            endpoint,
        } => {
            secrets.push((project.clone(), "project"));
            secrets.push((token.clone(), "access token"));
            let live = run_live(
                cases,
                &names,
                (project, token, endpoint),
                scratch,
                &proto_root,
            );
            // The project number identifies the project like its name does.
            secrets.extend(
                live.run
                    .project_numbers
                    .iter()
                    .map(|number| (number.clone(), "project number")),
            );
            reference_requests = Some(live.requests);
            reference_retries = count_retries(&live.run.cases);
            reference_cleanup = live.run.cleanup;
            reference_failed_cleanup = live.run.failed_cleanup;
            let recording = recording::Recording {
                generated: today(),
                requests: live.requests,
                tools: tools_note(),
                cases: live.run.cases.clone(),
            };
            let path = options
                .record
                .clone()
                .unwrap_or_else(|| out_dir(options, work).join("reference-recording.json"));
            write_checked(
                &path,
                &(recording::to_json(&recording).to_string() + "\n"),
                &secrets,
            )?;
            println!("wrote sanitized reference recording: {}", path.display());
            Some(live.run.cases)
        }
    };

    if let (Mode::SelfCheck, Some(path)) = (mode, &options.record) {
        // Recording gcs-mimic itself gives an offline fixture for testing replay.
        let recording = recording::Recording {
            generated: today(),
            requests: 0,
            tools: tools_note(),
            cases: mimic_run.cases.clone(),
        };
        write_checked(
            path,
            &(recording::to_json(&recording).to_string() + "\n"),
            &secrets,
        )?;
    }

    let reports = report::build(cases, &mimic_run.cases, reference_cases.as_deref());
    let mode_label = match mode {
        Mode::SelfCheck => "self-check (gcs-mimic only)",
        Mode::Live { .. } => "live comparison against the real service",
        Mode::Replay(_) => "replay of a recorded reference",
    };
    let header = report::Header {
        mode: mode_label,
        generated: &generated,
        reference_requests,
        reference_retries,
        cleanup: &mimic_run.cleanup,
        reference_cleanup: &reference_cleanup,
        skipped: &skipped,
    };
    let markdown = report::markdown(&header, &reports);
    let json_text = report::json_report(&header, &reports).to_string() + "\n";
    let out = out_dir(options, work);
    write_checked(&out.join("report.md"), &markdown, &secrets)?;
    write_checked(&out.join("report.json"), &json_text, &secrets)?;
    if let Some(path) = &options.evidence {
        write_checked(path, &markdown, &secrets)?;
    }
    println!("{markdown}");
    println!("reports: {}", out.display());
    let failed_cleanup = mimic_run.failed_cleanup || reference_failed_cleanup;
    Ok(failed_cleanup || report::has_failures(&reports, options.fail_on_diff))
}

/// Deletes every bucket whose name starts with the harness prefix, for the
/// leftovers of a run that was interrupted. Needs the live environment.
pub fn sweep(args: &[String]) -> Result<bool> {
    if !args.is_empty() {
        return Err("differential sweep takes no options".into());
    }
    let Mode::Live {
        project,
        token,
        endpoint,
    } = select_mode(&Options::default())?
    else {
        return Err("sweep needs GCS_MIMIC_DIFF_PROJECT and GCS_MIMIC_DIFF_TOKEN".into());
    };
    let scratch = env::temp_dir().join(format!("gcsmimic-sweep-{}", random_hex()));
    fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let curl = CurlTransport::new(&endpoint, Some(token.clone()), false, &scratch);
    let paced = Paced::new(&curl);
    let guarded = Guarded { inner: &paced };
    let side = Side {
        transport: &guarded,
        grpc: None,
        project: &project,
        hosts: live_hosts(&endpoint),
        server_error_wait: Duration::from_secs(30),
    };
    let mut norm = normalize::Normalizer::default();
    norm.add_literal(&project, "<project>");
    norm.add_literal(&token, "<token>");
    let (lines, failed) = bucket::sweep(&side, &norm, bucket::NAME_PREFIX);
    server::remove_dir(&scratch);
    for line in &lines {
        println!("{line}");
    }
    println!("{} requests sent to the live service", curl.requests());
    Ok(failed)
}

fn out_dir(options: &Options, work: &Path) -> PathBuf {
    options.out.clone().unwrap_or_else(|| work.join("out"))
}

/// Drops the gRPC cases when `grpcurl` is not installed, so the rest of the
/// run still counts. The report lists each dropped case as skipped.
fn split_unrunnable(cases: &[case::Case], grpcurl: bool) -> (Vec<case::Case>, Vec<String>) {
    let mut runnable = Vec::new();
    let mut skipped = Vec::new();
    for case in cases {
        if case.uses_grpc() && !grpcurl {
            skipped.push(format!("{}: grpcurl is not installed", case.id));
        } else {
            runnable.push(case.clone());
        }
    }
    (runnable, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case_from(id: &str, protocol: &str) -> case::Case {
        let step = if protocol == "grpc" {
            serde_json::json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C"})
        } else {
            serde_json::json!({"name": "s", "method": "GET", "path": "/p"})
        };
        let value = serde_json::json!({"id": id, "description": "d", "steps": [step]});
        case::parse_case(&value, id).unwrap()
    }

    #[test]
    fn a_leak_still_writes_the_redacted_file_and_fails_with_the_location() {
        let dir = env::temp_dir().join(format!("gcsmimic-wc-{}", random_hex()));
        let path = dir.join("out.json");
        let secrets = vec![("secret-proj".to_owned(), "project")];
        let text = r#"{"cases":[{"id":"c","steps":[{"name":"s","request":"GET /secret-proj"}]}]}"#;
        let error = write_checked(&path, text, &secrets).unwrap_err();
        assert!(
            error.contains("$.cases[c].steps[s].request: the project"),
            "{error}"
        );
        assert!(!error.contains("secret-proj"), "{error}");
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("secret-proj") && written.contains("<redacted>"));
        assert!(write_checked(&dir.join("ok.json"), "{}", &secrets).is_ok());
        server::remove_dir(&dir);
    }
    #[test]
    fn missing_grpcurl_skips_only_the_grpc_cases() {
        let cases = [case_from("a", "http"), case_from("b", "grpc")];
        let (runnable, skipped) = split_unrunnable(&cases, false);
        assert_eq!(runnable.len(), 1);
        assert_eq!(runnable[0].id, "a");
        assert_eq!(skipped, ["b: grpcurl is not installed"]);
        let (runnable, skipped) = split_unrunnable(&cases, true);
        assert_eq!(runnable.len(), 2);
        assert!(skipped.is_empty());
    }
}
