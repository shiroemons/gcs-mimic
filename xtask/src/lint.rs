//! Zero-tolerance code-quality gate for the server's production code.
//!
//! `cargo xtask lint` runs Clippy on the server library and binaries with [`CLIPPY_LINTS`] and
//! applies the text rules to production files. Any finding fails the gate. The only exceptions
//! are reasoned suppressions (`#[expect(lint, reason = "..")]` for Clippy and rustc lints,
//! `// xtask-lint: allow(text::..) -- reason` for text rules) and the one shared error-mapping
//! helper in [`SHARED_HELPER_FILE`]. `--target <triple>` repeats the Clippy pass for another
//! target, so code behind that target's `cfg` is checked too. Rules in [`PENDING_RULES`] are
//! reported as warnings until the code they reject is fixed. `--all-targets-report` lists the
//! same Clippy lints across every workspace target for information; tests may unwrap, so it
//! never fails.

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    ops::Range,
    path::Path,
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, String>;

/// Extra Clippy lints for production code; thresholds live in `clippy.toml`. A lint that passes
/// on every workspace target belongs in `[workspace.lints]` instead: a `-W` here would lower
/// that `deny` to a warning for this run.
pub const CLIPPY_LINTS: [&str; 9] = [
    "clippy::unwrap_used",
    "clippy::expect_used",
    "clippy::panic",
    "clippy::too_many_lines",
    "clippy::fn_params_excessive_bools",
    "clippy::needless_pass_by_value",
    "clippy::allow_attributes_without_reason",
    // `#[expect]` fails once its lint stops firing; `#[allow]` silently outlives the reason.
    ALLOW_ATTRIBUTES,
    // The methods in clippy.toml; `[workspace.lints]` allows it so tests may call them.
    "clippy::disallowed_methods",
];

const ALLOW_ATTRIBUTES: &str = "clippy::allow_attributes";

/// The only file that may define `fn unavailable`, the shared storage error mapping.
pub const SHARED_HELPER_FILE: &str = "src/storage/failure.rs";

const PATH_ATTRIBUTE: &str = "text::path_attribute";
const STRING_ERROR_RESULT: &str = "text::string_error_result";
const UNAVAILABLE_HELPER: &str = "text::unavailable_helper";
const ALLOW_TOO_MANY_ARGUMENTS: &str = "text::allow_too_many_arguments";
const UNCACHED_PREPARE: &str = "text::uncached_prepare";
const UNCACHED_STATEMENT: &str = "text::uncached_statement";
const FORMAT_SQL: &str = "text::format_sql";
const SYNC_WITHOUT_FLUSH: &str = "text::sync_without_flush";
const STRING_ERROR_VARIANT: &str = "text::string_error_variant";
const STRINGIFIED_ERROR: &str = "text::stringified_error";

/// Pseudo-rule for the findings of a `--target` run; listing it in [`PENDING_RULES`] reports
/// every cross-target finding as pending.
pub const CROSS_TARGET: &str = "target::cross";

/// Rules reported as warnings instead of failures while existing code is migrated. Each entry
/// is temporary: remove it (or empty the list) to enforce the rule once its findings are fixed.
/// A pending rule still accepts suppressions, and stale or invalid suppressions still fail.
pub const PENDING_RULES: &[&str] = &[];

/// Rules that `// xtask-lint: allow(..)` may suppress. Clippy and rustc lints use
/// `#[expect(lint, reason = "..")]` instead, which the compiler already checks for staleness.
pub const TEXT_RULES: [&str; 10] = [
    PATH_ATTRIBUTE,
    STRING_ERROR_RESULT,
    UNAVAILABLE_HELPER,
    ALLOW_TOO_MANY_ARGUMENTS,
    UNCACHED_PREPARE,
    UNCACHED_STATEMENT,
    FORMAT_SQL,
    SYNC_WITHOUT_FLUSH,
    STRING_ERROR_VARIANT,
    STRINGIFIED_ERROR,
];

/// Rules that `// xtask-lint: allow-file(..)` may suppress for a whole file: statements that no
/// API request reaches, such as schema setup, migrations and the sync mirror's worker, cluster
/// in modules where per-line comments would only repeat one reason.
pub const FILE_SCOPE_RULES: [&str; 1] = [UNCACHED_STATEMENT];

/// The request adapters, where every module serves API requests, so no file there is
/// request-free and a file-scope suppression would only hide request-path findings. A
/// directory also covers its `<dir>.rs` module root.
pub const FILE_SCOPE_FORBIDDEN_DIRS: [&str; 4] = ["src/server", "src/json", "src/xml", "src/grpc"];

fn file_scope_forbidden(file: &str) -> bool {
    FILE_SCOPE_FORBIDDEN_DIRS.iter().any(|dir| {
        file.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/') || rest == ".rs")
    })
}

const SUPPRESSION_MARKER: &str = "xtask-lint:";
const SUPPRESSION_SYNTAX: &str = "// xtask-lint: allow(<text rule>[, <text rule>]) -- <reason>";
const FILE_SUPPRESSION_SYNTAX: &str = "// xtask-lint: allow-file(<text rule>) -- <reason>";
/// Short enough for a real justification, long enough to reject `ok` or `legacy`.
pub const MIN_REASON_CHARS: usize = 10;

/// tokio's async writes; a `write(` without arguments is a lock guard, not I/O.
const ASYNC_WRITES: [&str; 5] = [
    "write_all",
    "write",
    "write_buf",
    "write_all_buf",
    "write_vectored",
];

const SQL_KEYWORDS: [&str; 10] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "REPLACE", "WITH", "CREATE", "DROP", "ALTER", "PRAGMA",
];
const SQL_MARKERS: [&str; 7] = [
    " FROM ", " INTO ", " SET ", " TABLE ", " INDEX ", " VALUES", "PRAGMA ",
];

/// rusqlite methods that prepare their SQL on every call, bypassing the statement cache.
/// `execute_batch` is left out: it runs multi-statement DDL and has no cached form.
const UNCACHED_METHODS: [&str; 3] = ["execute", "query_row", "query_one"];

/// Error-variant names that describe an underlying failure rather than a client-facing
/// message. `InvalidTopic(String)` names the rejected input and stays allowed;
/// `Backend(String)` is an error that was stringified and lost its `source()`.
const SOURCE_VARIANT_NAMES: [&str; 9] = [
    "Backend",
    "BodyAborted",
    "Database",
    "Internal",
    "Io",
    "Other",
    "Sqlite",
    "Storage",
    "Unavailable",
];

const STRING_TYPES: [&str; 3] = ["String", "std::string::String", "alloc::string::String"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Fail on any production finding.
    Gate,
    /// Run only the Clippy pass for another target triple; text rules are target-independent
    /// and already ran with the host gate.
    Target(String),
    /// Print the Clippy lints across every workspace target; never fails on findings.
    AllTargetsReport,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub rule: String,
    pub file: String,
    pub line: usize,
}

impl Finding {
    fn new(rule: &str, file: &str, line: usize) -> Self {
        Self {
            rule: rule.to_owned(),
            file: file.to_owned(),
            line,
        }
    }
}

/// A malformed, unknown, unreasoned or stale suppression comment; it always fails the gate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    pub file: String,
    pub line: usize,
    pub message: String,
}

/// Text-rule results for one or more files.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub findings: Vec<Finding>,
    /// Findings silenced by a reasoned suppression; reported so review sees them.
    pub suppressed: Vec<Finding>,
    /// Malformed, unknown, unreasoned or stale suppressions.
    pub problems: Vec<Problem>,
}

impl Scan {
    fn extend(&mut self, other: Self) {
        self.findings.extend(other.findings);
        self.suppressed.extend(other.suppressed);
        self.problems.extend(other.problems);
    }
}

/// Which code a Clippy pass compiles and which reported files it keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The server's library and binaries: the files the gate checks.
    Production,
    /// Every workspace target: what `[workspace.lints]` would reach.
    Workspace,
}

impl Scope {
    fn cargo_args(self) -> &'static [&'static str] {
        match self {
            // `--lib --bins` leaves `cfg(test)` code uncompiled, so test helpers never count.
            // xtask and the gRPC code generator are build tooling.
            Self::Production => &["--package", "gcs-mimic", "--lib", "--bins"],
            Self::Workspace => &["--workspace", "--all-targets"],
        }
    }

    fn includes(self, path: &str) -> bool {
        match self {
            Self::Production => production_file(path),
            Self::Workspace => path.ends_with(".rs") && !path.starts_with(".."),
        }
    }
}

/// Parses the arguments after `lint`.
pub fn mode(args: &[String]) -> Option<Mode> {
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => Some(Mode::Gate),
        ["--all-targets-report"] => Some(Mode::AllTargetsReport),
        ["--target", triple] if !triple.is_empty() && !triple.starts_with('-') => {
            Some(Mode::Target(triple.to_owned()))
        }
        _ => None,
    }
}

/// Runs the selected mode and returns whether it passed.
pub fn run(root: &Path, mode: &Mode) -> Result<bool> {
    match mode {
        Mode::Gate => gate(root),
        Mode::Target(triple) => Ok(target_gate(
            triple,
            clippy(root, Scope::Production, Some(triple))?,
        )),
        Mode::AllTargetsReport => {
            all_targets_report(&clippy(root, Scope::Workspace, None)?);
            Ok(true)
        }
    }
}

/// Splits findings into enforced ones and those of [`PENDING_RULES`]; with `cross_target`,
/// every finding is pending while [`CROSS_TARGET`] is.
pub fn split_pending(findings: Vec<Finding>, cross_target: bool) -> (Vec<Finding>, Vec<Finding>) {
    let all_pending = cross_target && PENDING_RULES.contains(&CROSS_TARGET);
    findings
        .into_iter()
        .partition(|f| !all_pending && !PENDING_RULES.contains(&f.rule.as_str()))
}

/// `N pending findings (rule: n, ..)` plus one `warning: pending` line per rule and file, so the
/// migration workload stays visible without failing the gate.
fn report_pending(pending: &[Finding]) -> String {
    for ((rule, file), count) in counts_by_file(pending) {
        eprintln!("warning: pending {rule}: {file}: {count}");
    }
    let mut by_rule = BTreeMap::<&str, usize>::new();
    for finding in pending {
        *by_rule.entry(&finding.rule).or_default() += 1;
    }
    let detail = by_rule
        .iter()
        .map(|(rule, n)| format!("{rule}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    match pending.len() {
        0 => "0 pending findings".to_owned(),
        n => format!("{n} pending findings, not enforced yet ({detail})"),
    }
}

fn target_gate(triple: &str, findings: Vec<Finding>) -> bool {
    let (violations, pending) = split_pending(findings, true);
    let pending = report_pending(&pending);
    for finding in &violations {
        eprintln!("error: {}: {}:{}", finding.rule, finding.file, finding.line);
    }
    if violations.is_empty() {
        println!("lint --target {triple}: 0 production Clippy violations; {pending}");
    } else {
        eprintln!(
            "lint --target {triple} failed: {} production Clippy violations; {pending}",
            violations.len()
        );
    }
    violations.is_empty()
}

fn gate(root: &Path) -> Result<bool> {
    let mut findings = clippy(root, Scope::Production, None)?;
    let scan = scan_tree(root)?;
    findings.extend(scan.findings);
    findings.sort();
    let (findings, pending) = split_pending(findings, false);
    let pending = report_pending(&pending);
    let suppressions = suppression_summary(&scan.suppressed);
    for problem in &scan.problems {
        eprintln!(
            "error: {}:{}: {}",
            problem.file, problem.line, problem.message
        );
    }
    if !scan.problems.is_empty() {
        eprintln!(
            "Suppressions use `{SUPPRESSION_SYNTAX}` or `{FILE_SUPPRESSION_SYNTAX}`; see docs/en/development.md."
        );
    }
    let (violations, missing_helper) = violations(findings);
    for finding in &violations {
        eprintln!("error: {}: {}:{}", finding.rule, finding.file, finding.line);
    }
    if let Some(message) = &missing_helper {
        eprintln!("error: {message}");
    }
    let passed = violations.is_empty() && missing_helper.is_none() && scan.problems.is_empty();
    if passed {
        println!("lint: 0 production violations; {suppressions}; {pending}");
    } else {
        eprintln!(
            "lint failed: {} production violations, {} suppression problems; {suppressions}; {pending}",
            violations.len() + usize::from(missing_helper.is_some()),
            scan.problems.len()
        );
        eprintln!(
            "Fix each finding, or justify it with `#[expect(<lint>, reason = \"...\")]` or `{SUPPRESSION_SYNTAX}`."
        );
    }
    Ok(passed)
}

/// Gate violations: every finding except the single shared `fn unavailable` in
/// [`SHARED_HELPER_FILE`], plus a message when that file does not define it exactly once.
/// Two definitions there are both reported, so a duplicate cannot hide behind the original.
pub fn violations(findings: Vec<Finding>) -> (Vec<Finding>, Option<String>) {
    let shared = |f: &Finding| f.rule == UNAVAILABLE_HELPER && f.file == SHARED_HELPER_FILE;
    let definitions = findings.iter().filter(|f| shared(f)).count();
    let violations = findings
        .into_iter()
        .filter(|f| definitions != 1 || !shared(f))
        .collect();
    let missing = (definitions == 0).then(|| {
        format!(
            "{UNAVAILABLE_HELPER}: {SHARED_HELPER_FILE} must define the shared `fn unavailable`"
        )
    });
    (violations, missing)
}

/// `N reasoned suppressions (rule: n, ..)`, so reviewers see accepted exceptions next to counts.
pub fn suppression_summary(suppressed: &[Finding]) -> String {
    let mut by_rule = BTreeMap::<&str, usize>::new();
    for finding in suppressed {
        *by_rule.entry(&finding.rule).or_default() += 1;
    }
    let detail = by_rule
        .iter()
        .map(|(rule, n)| format!("{rule}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    match suppressed.len() {
        0 => "0 reasoned suppressions".to_owned(),
        n => format!("{n} reasoned suppressions ({detail})"),
    }
}

/// Prints the [`CLIPPY_LINTS`] findings of every workspace target per lint and file, then the
/// lints with none, which can move to `[workspace.lints]` as `deny`.
fn all_targets_report(workspace: &[Finding]) {
    for ((rule, file), count) in counts_by_file(workspace) {
        println!("{rule}\t{file}\t{count}");
    }
    let ready = ready_lints(workspace);
    println!(
        "all targets: {} findings (informational; tests may unwrap)",
        workspace.len()
    );
    println!(
        "lints ready to deny in [workspace.lints]: {}",
        if ready.is_empty() {
            "none".to_owned()
        } else {
            ready.join(", ")
        }
    );
}

/// Finding counts per `(rule, file)`, sorted.
pub fn counts_by_file(findings: &[Finding]) -> BTreeMap<(String, String), usize> {
    let mut counts = BTreeMap::new();
    for finding in findings {
        *counts
            .entry((finding.rule.clone(), finding.file.clone()))
            .or_default() += 1;
    }
    counts
}

/// [`CLIPPY_LINTS`] with no finding on any workspace target.
pub fn ready_lints(workspace: &[Finding]) -> Vec<&'static str> {
    CLIPPY_LINTS
        .into_iter()
        .filter(|lint| !workspace.iter().any(|f| f.rule == *lint))
        .collect()
}

fn clippy(root: &Path, scope: Scope, target: Option<&str>) -> Result<Vec<Finding>> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(root)
        .arg("clippy")
        .args(scope.cargo_args())
        .args(["--locked", "--offline", "--message-format=json"]);
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    command.arg("--");
    for lint in CLIPPY_LINTS {
        command.args(["-W", lint]);
    }
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cargo clippy: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        // Compiler errors arrive on stdout as JSON, so surface them instead of a bare status.
        let rendered: String = stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|value| value["message"]["level"] == "error")
            .filter_map(|value| value["message"]["rendered"].as_str().map(str::to_owned))
            .collect();
        return Err(format!(
            "cargo clippy failed ({})\n{rendered}",
            output.status
        ));
    }
    Ok(clippy_findings(&stdout, scope))
}

/// Extracts curated lint findings on the scope's files from a `--message-format=json` stream.
/// Several targets can report the same span, so findings are de-duplicated.
pub fn clippy_findings(stream: &str, scope: Scope) -> Vec<Finding> {
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for line in stream.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-message" {
            continue;
        }
        let message = &value["message"];
        let Some(code) = message["code"]["code"].as_str() else {
            continue;
        };
        if !CLIPPY_LINTS.contains(&code) {
            continue;
        }
        let Some(span) = message["spans"]
            .as_array()
            .and_then(|spans| spans.iter().find(|s| s["is_primary"] == true))
        else {
            continue;
        };
        let raw = span["file_name"].as_str().unwrap_or_default();
        // rustc names workspace files relative to the root and dependency sources absolutely.
        if raw.starts_with(['/', '\\']) || raw.contains(':') {
            continue;
        }
        let file = normalize(raw);
        let line = span["line_start"].as_u64().unwrap_or_default();
        let column = span["column_start"].as_u64().unwrap_or_default();
        if !scope.includes(&file) || !seen.insert((code.to_owned(), file.clone(), line, column)) {
            continue;
        }
        findings.push(Finding::new(
            code,
            &file,
            usize::try_from(line).unwrap_or(usize::MAX),
        ));
    }
    findings
}

/// Resolves `.`/`..` lexically: `#[path]` and `include!` make rustc report spans such as
/// `src/grpc/v1/../generated/x.rs`, which must match the scanned file names.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "." | "" => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Whether a repository-relative path is server production code.
pub fn production_file(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().unwrap_or(path);
    // Children of a `tests` or `*_tests` module directory belong to that test module.
    let test_directory = parts
        .iter()
        .any(|part| *part == "tests" || part.ends_with("_tests"));
    path.starts_with("src/")
        && path.ends_with(".rs")
        && !path.starts_with("src/grpc/generated/")
        && !path.starts_with("src/grpc/codegen/")
        && !test_directory
        && !name.ends_with("_tests.rs")
        && name != "tests.rs"
        && name != "test_support.rs"
}

fn scan_tree(root: &Path) -> Result<Scan> {
    let mut files = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(directory) = pending.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|e| format!("{}: {e}", directory.display()))?
        {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(root) {
                let relative = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if production_file(&relative) {
                    files.push((relative, path));
                }
            }
        }
    }
    files.sort();
    let mut scan = Scan::default();
    for (relative, path) in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?;
        scan.extend(scan_source(&relative, &text));
    }
    Ok(scan)
}

/// Applies the text rules to one production file, skipping `#[cfg(test)]` items, then
/// applies its suppression comments.
pub fn scan_source(file: &str, text: &str) -> Scan {
    let (masked, comments) = mask_with_comments(text);
    let findings = text_findings(file, text, &masked);
    let mut suppressions = Vec::new();
    let mut problems = Vec::new();
    for comment in comments {
        match parse_suppression(&text[comment.clone()]) {
            None => {}
            Some(Err(message)) => problems.push(Problem {
                file: file.to_owned(),
                line: line_index(text, comment.start) + 1,
                message,
            }),
            Some(Ok(parsed))
                if parsed.scope == SuppressionScope::File && file_scope_forbidden(file) =>
            {
                problems.push(Problem {
                    file: file.to_owned(),
                    line: line_index(text, comment.start) + 1,
                    message: format!(
                        "`allow-file` is not accepted in the request adapters ({}); use line suppressions",
                        FILE_SCOPE_FORBIDDEN_DIRS.join(", ")
                    ),
                });
            }
            Some(Ok(parsed)) => suppressions.push(Suppression {
                comment: line_index(text, comment.start),
                target: match parsed.scope {
                    SuppressionScope::Line => suppression_target(&masked, comment.start),
                    SuppressionScope::File => None,
                },
                scope: parsed.scope,
                used: vec![false; parsed.rules.len()],
                rules: parsed.rules,
            }),
        }
    }
    let mut scan = Scan {
        problems,
        ..Scan::default()
    };
    // Line suppressions first, so a file-wide one is stale only when no line one covers it.
    suppressions.sort_by_key(|s| s.scope);
    for finding in findings {
        let matched = suppressions.iter_mut().find_map(|s| {
            let index = s.rules.iter().position(|rule| *rule == finding.rule)?;
            let covers = match s.scope {
                SuppressionScope::Line => s.target == Some(finding.line - 1),
                SuppressionScope::File => true,
            };
            covers.then_some((s, index))
        });
        if let Some((suppression, index)) = matched {
            suppression.used[index] = true;
            scan.suppressed.push(finding);
        } else {
            scan.findings.push(finding);
        }
    }
    for suppression in &suppressions {
        for (rule, used) in suppression.rules.iter().zip(&suppression.used) {
            if !used {
                // Like an unfulfilled `#[expect]`: a fix or a moved line must drop the comment.
                let covered = match suppression.scope {
                    SuppressionScope::Line => "the line it covers",
                    SuppressionScope::File => "this file",
                };
                scan.problems.push(Problem {
                    file: file.to_owned(),
                    line: suppression.comment + 1,
                    message: format!("stale suppression: `{rule}` reports nothing on {covered}"),
                });
            }
        }
    }
    scan.problems.sort();
    scan
}

/// What a suppression comment covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuppressionScope {
    /// `allow(..)`: one line.
    Line,
    /// `allow-file(..)`: the whole file, for [`FILE_SCOPE_RULES`] only.
    File,
}

/// The accepted contents of a suppression comment.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedSuppression {
    pub scope: SuppressionScope,
    pub rules: Vec<String>,
}

/// A parsed `// xtask-lint: allow(..) -- reason` or `allow-file(..)` comment.
struct Suppression {
    /// Zero-based line of the comment.
    comment: usize,
    /// Zero-based line a line suppression covers: its own line after code, else the next line
    /// with code. `None` for a file suppression.
    target: Option<usize>,
    scope: SuppressionScope,
    rules: Vec<String>,
    used: Vec<bool>,
}

fn line_index(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count()
}

/// The zero-based line a suppression comment starting at `offset` covers.
fn suppression_target(masked: &str, offset: usize) -> Option<usize> {
    let line = line_index(masked, offset);
    let line_start = masked[..offset].rfind('\n').map_or(0, |n| n + 1);
    if !masked[line_start..offset].trim().is_empty() {
        return Some(line);
    }
    masked
        .lines()
        .enumerate()
        .skip(line + 1)
        .find(|(_, code)| !code.trim().is_empty())
        .map(|(index, _)| index)
}

/// Parses one line comment (including its `//`). `None` when it is not a suppression;
/// otherwise the suppressed rules and scope, or why the comment is rejected.
pub fn parse_suppression(comment: &str) -> Option<std::result::Result<ParsedSuppression, String>> {
    let body = comment.strip_prefix("//")?;
    // Doc comments document; they never suppress.
    if body.starts_with('/') || body.starts_with('!') {
        return None;
    }
    let rest = body.trim_start().strip_prefix(SUPPRESSION_MARKER)?;
    let malformed = || {
        Err(format!(
            "malformed suppression; expected `{SUPPRESSION_SYNTAX}` or `{FILE_SUPPRESSION_SYNTAX}`"
        ))
    };
    let rest = rest.trim_start();
    let (scope, rest) = if let Some(rest) = rest.strip_prefix("allow(") {
        (SuppressionScope::Line, rest)
    } else if let Some(rest) = rest.strip_prefix("allow-file(") {
        (SuppressionScope::File, rest)
    } else {
        return Some(malformed());
    };
    let Some((list, after)) = rest.split_once(')') else {
        return Some(malformed());
    };
    let rules: Vec<String> = list.split(',').map(|r| r.trim().to_owned()).collect();
    if rules.iter().any(String::is_empty) {
        return Some(malformed());
    }
    for rule in &rules {
        // Compiler lints already reject stale or reasonless expectations themselves.
        if !rule.starts_with("text::") {
            return Some(Err(format!(
                "`{rule}` cannot be suppressed by a comment; use `#[expect({rule}, reason = \"...\")]`"
            )));
        }
        if !TEXT_RULES.contains(&rule.as_str()) {
            return Some(Err(format!(
                "unknown rule `{rule}`; suppressible rules: {}",
                TEXT_RULES.join(", ")
            )));
        }
        if scope == SuppressionScope::File && !FILE_SCOPE_RULES.contains(&rule.as_str()) {
            return Some(Err(format!(
                "`{rule}` cannot be suppressed for a whole file; file-scope rules: {}",
                FILE_SCOPE_RULES.join(", ")
            )));
        }
    }
    let after = after.trim();
    let reason = match after.strip_prefix("--") {
        Some(reason) => reason.trim(),
        None if after.is_empty() => "",
        None => return Some(malformed()),
    };
    if reason.chars().count() < MIN_REASON_CHARS {
        return Some(Err(format!(
            "suppression needs a reason of at least {MIN_REASON_CHARS} characters after ` -- `"
        )));
    }
    Some(Ok(ParsedSuppression { scope, rules }))
}

/// Raw text-rule findings in production code, before suppressions.
fn text_findings(file: &str, text: &str, masked: &str) -> Vec<Finding> {
    let tests = test_lines(masked);
    let line_of = |offset: usize| masked[..offset].matches('\n').count();
    let mut findings = Vec::new();
    for (index, line) in masked.lines().enumerate() {
        if tests.get(index).copied().unwrap_or(false) {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[path ") || trimmed.starts_with("#[path=") {
            findings.push(Finding::new(PATH_ATTRIBUTE, file, index + 1));
        }
        for _ in line.matches(".prepare(") {
            findings.push(Finding::new(UNCACHED_PREPARE, file, index + 1));
        }
    }
    let mut add = |rule: &str, offset: usize| {
        let line = line_of(offset);
        if !tests.get(line).copied().unwrap_or(false) {
            findings.push(Finding::new(rule, file, line + 1));
        }
    };
    for offset in words(masked, "fn") {
        if let Some((name, returns)) = signature(masked, offset) {
            if name == "unavailable" {
                add(UNAVAILABLE_HELPER, offset);
            }
            if returns.is_some_and(string_error_result) {
                add(STRING_ERROR_RESULT, offset);
            }
        }
    }
    for offset in words(masked, "type") {
        if alias(masked, offset).is_some_and(string_error_result) {
            add(STRING_ERROR_RESULT, offset);
        }
    }
    for offset in words(masked, "format") {
        if format_sql(text, masked, offset) {
            add(FORMAT_SQL, offset);
        }
    }
    for offset in unflushed_syncs(masked) {
        add(SYNC_WITHOUT_FLUSH, offset);
    }
    for offset in too_many_arguments_attributes(masked) {
        add(ALLOW_TOO_MANY_ARGUMENTS, offset);
    }
    for offset in uncached_statements(masked) {
        add(UNCACHED_STATEMENT, offset);
    }
    for offset in string_error_variants(masked) {
        add(STRING_ERROR_VARIANT, offset);
    }
    for offset in stringified_errors(text, masked) {
        add(STRINGIFIED_ERROR, offset);
    }
    findings
}

/// Offsets of `#[..]` and `#![..]` attributes that allow or expect `clippy::too_many_arguments`,
/// including `cfg_attr` forms and attributes split over several lines.
fn too_many_arguments_attributes(masked: &str) -> Vec<usize> {
    let bytes = masked.as_bytes();
    let mut offsets = Vec::new();
    for (hash, _) in masked.match_indices('#') {
        let mut open = hash + 1;
        if bytes.get(open) == Some(&b'!') {
            open += 1;
        }
        if bytes.get(open) != Some(&b'[') {
            continue;
        }
        let Some(close) = close_group(bytes, open) else {
            continue;
        };
        let body: String = masked[open + 1..close - 1]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let level = ["allow(", "expect(", "cfg_attr("]
            .iter()
            .any(|prefix| body.starts_with(prefix));
        if level && words(&body, "too_many_arguments").any(|i| body[..i].ends_with("clippy::")) {
            offsets.push(hash);
        }
    }
    offsets
}

/// Whether the last non-whitespace byte before `offset` is `.`, so a method call follows.
fn after_dot(bytes: &[u8], offset: usize) -> bool {
    bytes[..offset]
        .iter()
        .rev()
        .find(|b| !b.is_ascii_whitespace())
        == Some(&b'.')
}

/// Offsets of `.execute(`, `.query_row(` and `.query_one(` whose first argument is a string
/// literal or an `UPPER_SNAKE` constant: rusqlite prepares that SQL again on every call.
/// A call on a prepared statement passes parameters first and is not matched.
fn uncached_statements(masked: &str) -> Vec<usize> {
    let bytes = masked.as_bytes();
    let mut offsets = Vec::new();
    for method in UNCACHED_METHODS {
        for offset in words(masked, method) {
            if !after_dot(bytes, offset) {
                continue;
            }
            let open = skip_space(bytes, offset + method.len());
            if bytes.get(open) != Some(&b'(') {
                continue;
            }
            let mut i = skip_space(bytes, open + 1);
            if bytes.get(i) == Some(&b'&') {
                i = skip_space(bytes, i + 1);
            }
            let literal = bytes.get(i) == Some(&b'"')
                || (bytes.get(i) == Some(&b'r') && matches!(bytes.get(i + 1), Some(b'"' | b'#')));
            let start = i;
            while bytes.get(i).is_some_and(|&b| ident(b) || b == b':') {
                i += 1;
            }
            let segment = masked[start..i].rsplit("::").next().unwrap_or_default();
            let constant = segment.bytes().any(|b| b.is_ascii_uppercase())
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                && matches!(bytes.get(skip_space(bytes, i)), Some(b',' | b')'));
            if literal || constant {
                offsets.push(offset);
            }
        }
    }
    offsets
}

/// The identifier starting at `start`, possibly empty.
fn ident_at(masked: &str, start: usize) -> &str {
    let bytes = masked.as_bytes();
    let mut end = start;
    while bytes.get(end).copied().is_some_and(ident) {
        end += 1;
    }
    &masked[start..end]
}

/// Offsets of tuple variants `Name(String)` in enums named `*Error` whose name, from
/// [`SOURCE_VARIANT_NAMES`], says they hold an underlying failure as text.
fn string_error_variants(masked: &str) -> Vec<usize> {
    let bytes = masked.as_bytes();
    let mut offsets = Vec::new();
    for offset in words(masked, "enum") {
        let start = skip_space(bytes, offset + "enum".len());
        let name = ident_at(masked, start);
        if !name.ends_with("Error") {
            continue;
        }
        let Some(open) = masked[start..].find('{').map(|n| start + n) else {
            continue;
        };
        let Some(close) = close_group(bytes, open) else {
            continue;
        };
        let mut i = open + 1;
        while i < close - 1 {
            match bytes[i] {
                // Attributes, struct variants and discriminants never hold a tuple variant.
                b'(' | b'[' | b'{' | b'<' => i = close_group(bytes, i).unwrap_or(close),
                b if ident(b) && !ident(bytes[i - 1]) => {
                    let variant = ident_at(masked, i);
                    let after = i + variant.len();
                    let paren = skip_space(bytes, after);
                    if bytes.get(paren) == Some(&b'(')
                        && SOURCE_VARIANT_NAMES.contains(&variant)
                        && let Some(end) = close_group(bytes, paren)
                    {
                        let field: String = masked[paren + 1..end - 1]
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect();
                        if STRING_TYPES.contains(&field.trim_end_matches(',')) {
                            offsets.push(i);
                        }
                    }
                    i = after;
                }
                _ => i += 1,
            }
        }
    }
    offsets
}

/// Offsets of `map_err(|e| Path(e.to_string()))` and `map_err(|e| Path(format!(".. {e}")))`,
/// where the only argument stringifies the error and drops it as `source()`. A call that also
/// passes the error, such as `lock_failure(format!(..), e)`, keeps it and is not matched.
fn stringified_errors(text: &str, masked: &str) -> Vec<usize> {
    let bytes = masked.as_bytes();
    let mut offsets = Vec::new();
    for offset in words(masked, "map_err") {
        let open = skip_space(bytes, offset + "map_err".len());
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let bar = skip_space(bytes, open + 1);
        if bytes.get(bar) != Some(&b'|') {
            continue;
        }
        let param_start = skip_space(bytes, bar + 1);
        let param = ident_at(masked, param_start);
        let after_param = skip_space(bytes, param_start + param.len());
        if param.is_empty() || param.starts_with('_') || bytes.get(after_param) != Some(&b'|') {
            continue;
        }
        let path_start = skip_space(bytes, after_param + 1);
        let mut i = path_start;
        while bytes.get(i).is_some_and(|&b| ident(b) || b == b':') {
            i += 1;
        }
        let call = skip_space(bytes, i);
        if i == path_start || bytes.get(call) != Some(&b'(') {
            continue;
        }
        let Some(end) = close_group(bytes, call) else {
            continue;
        };
        if bytes.get(skip_space(bytes, end)) != Some(&b')') {
            continue;
        }
        let argument = masked[call + 1..end - 1].trim();
        let compact: String = argument.chars().filter(|c| !c.is_whitespace()).collect();
        if compact == format!("{param}.to_string()")
            || formats_param(text, masked, call + 1, end - 1, param)
        {
            offsets.push(offset);
        }
    }
    offsets
}

/// Whether `masked[from..to]` is exactly one `format!(..)` that interpolates `param`.
fn formats_param(text: &str, masked: &str, from: usize, to: usize, param: &str) -> bool {
    let bytes = masked.as_bytes();
    let start = skip_space(bytes, from);
    if !masked[start..to].starts_with("format!") {
        return false;
    }
    let open = skip_space(bytes, start + "format!".len());
    let Some(close) = close_group(bytes, open) else {
        return false;
    };
    if !masked[close..to]
        .trim()
        .trim_start_matches(',')
        .trim()
        .is_empty()
    {
        return false;
    }
    let literal = &text[open..close];
    let inline =
        literal.contains(&format!("{{{param}}}")) || literal.contains(&format!("{{{param}:"));
    inline || words(&masked[open..close], param).next().is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileEvent {
    /// A function item starts: write state never carries across functions.
    Function,
    Write,
    Flush,
    Sync,
}

/// Offsets of `.sync_all().await` / `.sync_data().await` that follow an awaited tokio write
/// in the same function with no `.flush().await` (or `.shutdown().await`) in between.
///
/// tokio's `File` completes its last write in the background; `sync_all` and `sync_data`
/// wait for it but keep its error to themselves and report only the fsync result, while
/// `flush` returns it. A failed tail write would otherwise publish a short file as durable.
/// Calls without `.await` are `std::fs::File`, whose writes already returned their errors.
/// The heuristic follows source order and does not track receivers, so a write to another
/// stream followed by a file sync is also reported.
fn unflushed_syncs(masked: &str) -> Vec<usize> {
    let bytes = masked.as_bytes();
    let mut events = Vec::new();
    for offset in words(masked, "fn") {
        if signature(masked, offset).is_some() {
            events.push((offset, FileEvent::Function));
        }
    }
    for (dot, _) in masked.match_indices('.') {
        let start = dot + 1;
        let mut end = start;
        while bytes.get(end).copied().is_some_and(ident) {
            end += 1;
        }
        let method = &masked[start..end];
        let event = if ASYNC_WRITES.contains(&method) {
            FileEvent::Write
        } else if matches!(method, "flush" | "shutdown") {
            FileEvent::Flush
        } else if matches!(method, "sync_all" | "sync_data") {
            FileEvent::Sync
        } else {
            continue;
        };
        let open = skip_space(bytes, end);
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let empty = bytes.get(skip_space(bytes, open + 1)) == Some(&b')');
        // `lock.write().await` takes a tokio `RwLock` guard; real writes pass a buffer.
        if event == FileEvent::Write && empty {
            continue;
        }
        let Some(close) = close_group(bytes, open) else {
            continue;
        };
        if !masked[skip_space(bytes, close)..].starts_with(".await") {
            continue;
        }
        events.push((dot, event));
    }
    events.sort_unstable_by_key(|(offset, _)| *offset);
    let mut written = false;
    let mut offsets = Vec::new();
    for (offset, event) in events {
        match event {
            FileEvent::Function | FileEvent::Flush => written = false,
            FileEvent::Write => written = true,
            // The background error stays pending after a sync, so a later sync is no better.
            FileEvent::Sync if written => offsets.push(offset),
            FileEvent::Sync => {}
        }
    }
    offsets
}

fn ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Offsets of `word` as a whole identifier.
fn words<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = text.as_bytes();
    text.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        (i == 0 || !ident(bytes[i - 1])) && bytes.get(i + word.len()).is_none_or(|&b| !ident(b))
    })
}

fn skip_space(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Index just past the bracket group opening at `i`; `->` never closes an angle bracket.
fn close_group(bytes: &[u8], i: usize) -> Option<usize> {
    let mut depth = 0usize;
    for j in i..bytes.len() {
        match bytes[j] {
            b'(' | b'[' | b'<' | b'{' => depth += 1,
            b'>' if j > 0 && bytes[j - 1] == b'-' => {}
            b')' | b']' | b'>' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(j + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The function name and declared return type of an item `fn` at `offset` in masked text.
fn signature(masked: &str, offset: usize) -> Option<(&str, Option<&str>)> {
    let bytes = masked.as_bytes();
    let start = skip_space(bytes, offset + 2);
    let mut end = start;
    while bytes.get(end).copied().is_some_and(ident) {
        end += 1;
    }
    // `fn(` is a function pointer type, not an item.
    if end == start {
        return None;
    }
    let name = &masked[start..end];
    let mut i = skip_space(bytes, end);
    if bytes.get(i) == Some(&b'<') {
        i = skip_space(bytes, close_group(bytes, i)?);
    }
    if bytes.get(i) != Some(&b'(') {
        return None;
    }
    i = skip_space(bytes, close_group(bytes, i)?);
    if !masked[i..].starts_with("->") {
        return Some((name, None));
    }
    Some((name, Some(type_until(masked, i + 2))))
}

/// The aliased type of a `type Name<..> = Type;` item at `offset` in masked text.
fn alias(masked: &str, offset: usize) -> Option<&str> {
    let bytes = masked.as_bytes();
    let mut i = skip_space(bytes, offset + 4);
    let start = i;
    while bytes.get(i).copied().is_some_and(ident) {
        i += 1;
    }
    if i == start {
        return None;
    }
    i = skip_space(bytes, i);
    if bytes.get(i) == Some(&b'<') {
        i = skip_space(bytes, close_group(bytes, i)?);
    }
    (bytes.get(i) == Some(&b'=')).then(|| type_until(masked, i + 1))
}

/// Type text from `start` up to a depth-zero `{`, `;` or `where`.
fn type_until(masked: &str, start: usize) -> &str {
    let bytes = masked.as_bytes();
    let mut depth = 0usize;
    for j in start..bytes.len() {
        match bytes[j] {
            b'(' | b'[' | b'<' => depth += 1,
            b'>' if bytes[j - 1] == b'-' => {}
            b')' | b']' | b'>' => depth = depth.saturating_sub(1),
            b'{' | b';' if depth == 0 => return &masked[start..j],
            b'w' if depth == 0
                && !ident(bytes[j - 1])
                && masked[j..].starts_with("where")
                && bytes.get(j + 5).is_none_or(|&b| !ident(b)) =>
            {
                return &masked[start..j];
            }
            _ => {}
        }
    }
    &masked[start..]
}

/// Whether a type is `Result<T, String>` under any path.
fn string_error_result(ty: &str) -> bool {
    let ty: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(open) = ty.find('<') else {
        return false;
    };
    if ty[..open].rsplit("::").next() != Some("Result") || !ty.ends_with('>') {
        return false;
    }
    let inner = &ty.as_bytes()[open + 1..ty.len() - 1];
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (j, &byte) in inner.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'<' => depth += 1,
            b'>' if j > 0 && inner[j - 1] == b'-' => {}
            b')' | b']' | b'>' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                args.push(&inner[start..j]);
                start = j + 1;
            }
            _ => {}
        }
    }
    args.push(&inner[start..]);
    args.len() == 2
        && matches!(
            args[1],
            b"String" | b"std::string::String" | b"alloc::string::String"
        )
}

/// Whether the `format` at `offset` is `format!` whose first argument is an SQL statement.
fn format_sql(text: &str, masked: &str, offset: usize) -> bool {
    let bytes = masked.as_bytes();
    let mut i = offset + "format".len();
    if bytes.get(i) != Some(&b'!') {
        return false;
    }
    i = skip_space(bytes, i + 1);
    if !matches!(bytes.get(i), Some(b'(' | b'[' | b'{')) {
        return false;
    }
    i = skip_space(bytes, i + 1);
    if bytes.get(i) == Some(&b'r') {
        i += 1;
        while bytes.get(i) == Some(&b'#') {
            i += 1;
        }
    }
    if bytes.get(i) != Some(&b'"') {
        return false;
    }
    // Masking blanks the literal's contents, so the next quote in the masked text closes it.
    let Some(close) = masked[i + 1..].find('"') else {
        return false;
    };
    let literal: String = text[i + 1..i + 1 + close]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let literal = format!("{literal} ");
    SQL_KEYWORDS
        .iter()
        .any(|keyword| literal.starts_with(&format!("{keyword} ")))
        && SQL_MARKERS.iter().any(|marker| literal.contains(marker))
}

/// Replaces comments and the contents of string and character literals with spaces, keeping
/// every byte offset and newline, so text rules never match inside them.
pub fn mask(text: &str) -> String {
    mask_with_comments(text).0
}

/// [`mask`], plus the byte ranges of line comments (from `//` to the line end), which carry
/// suppressions and must be read before masking hides them.
fn mask_with_comments(text: &str) -> (String, Vec<Range<usize>>) {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut comments = Vec::new();
    let blank = |out: &mut [u8], from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                blank(&mut out, i, end);
                comments.push(i..end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                let mut j = i;
                while j < bytes.len() {
                    if text[j..].starts_with("/*") {
                        depth += 1;
                        j += 2;
                    } else if text[j..].starts_with("*/") {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            b'r' if (i == 0
                || !ident(bytes[i - 1])
                || (bytes[i - 1] == b'b' && (i == 1 || !ident(bytes[i - 2]))))
                && matches!(bytes.get(i + 1), Some(b'"' | b'#')) =>
            {
                let mut j = i + 1;
                while bytes.get(j) == Some(&b'#') {
                    j += 1;
                }
                if bytes.get(j) != Some(&b'"') {
                    // A raw identifier such as `r#type`.
                    i = j;
                    continue;
                }
                let terminator = format!("\"{}", "#".repeat(j - i - 1));
                let end = text[j + 1..]
                    .find(&terminator)
                    .map_or(bytes.len(), |n| j + 1 + n);
                blank(&mut out, j + 1, end);
                i = (end + terminator.len()).min(bytes.len());
            }
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                let end = j.min(bytes.len());
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            b'\'' => {
                let next = text[i + 1..].chars().next();
                let width = next.map_or(0, char::len_utf8);
                if next == Some('\\') {
                    // Skip the escaped character so `'\''` ends at its last quote.
                    let end = text
                        .get(i + 3..)
                        .and_then(|rest| rest.find('\''))
                        .map_or(bytes.len(), |n| i + 3 + n);
                    blank(&mut out, i + 1, end);
                    i = end + 1;
                } else if width > 0 && bytes.get(i + 1 + width) == Some(&b'\'') {
                    blank(&mut out, i + 1, i + 1 + width);
                    i += 2 + width;
                } else {
                    // A lifetime or label.
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), comments)
}

/// Whether an attribute compiles its item only under test: `cfg(test)` or `cfg(all(.., test, ..))`.
fn test_only(attribute: &str) -> bool {
    attribute == "#[cfg(test)]"
        || attribute
            .strip_prefix("#[cfg(all(")
            .and_then(|rest| rest.strip_suffix("))]"))
            .is_some_and(|items| items.split(',').any(|item| item.trim() == "test"))
}

/// The zero-based line of the first outer attribute in the stack that ends just before
/// `line`, or `line` itself; attributes may span several lines.
fn attribute_stack_start(masked: &str, line: usize) -> usize {
    let Some(previous) = line.checked_sub(1) else {
        return 0;
    };
    let bytes = masked.as_bytes();
    let mut offset = masked
        .match_indices('\n')
        .nth(previous)
        .map_or(0, |(n, _)| n + 1);
    loop {
        let Some(end) = bytes[..offset]
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
        else {
            return line_index(masked, offset);
        };
        if bytes[end] != b']' {
            return line_index(masked, offset);
        }
        let mut depth = 0usize;
        let mut open = None;
        for j in (0..=end).rev() {
            match bytes[j] {
                b']' | b')' => depth += 1,
                b'[' | b'(' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        open = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        // Only `#[..]` outer attributes belong to the item; `#![..]` and indexing do not.
        match open {
            Some(j) if j > 0 && bytes[j - 1] == b'#' => offset = j - 1,
            _ => return line_index(masked, offset),
        }
    }
}

/// Marks the lines of each test-only item, from the first attribute of its stack (which may
/// precede the `cfg`) through its balanced end.
pub fn test_lines(masked: &str) -> Vec<bool> {
    let lines: Vec<&str> = masked.lines().collect();
    let mut tests = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if !test_only(lines[i].trim()) {
            i += 1;
            continue;
        }
        let start = attribute_stack_start(masked, i);
        i += 1;
        while i < lines.len() && (lines[i].trim().starts_with("#[") || lines[i].trim().is_empty()) {
            i += 1;
        }
        let mut depth = 0i64;
        while i < lines.len() {
            for byte in lines[i].bytes() {
                match byte {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth -= 1,
                    _ => {}
                }
            }
            i += 1;
            if depth <= 0 {
                break;
            }
        }
        tests[start..i].fill(true);
    }
    tests
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(file: &str, text: &str) -> Vec<(String, usize)> {
        scan_source(file, text)
            .findings
            .into_iter()
            .map(|f| (f.rule, f.line))
            .collect()
    }

    #[test]
    fn masking_keeps_offsets_and_hides_comments_and_literals() {
        let text = "let a = \"#[path = x] .prepare(\"; // .prepare(\n/* fn unavailable() */ let b = 'x'; let c = r#\"fn x\"#; 'a: loop {}\n";
        let masked = mask(text);
        assert_eq!(masked.len(), text.len());
        assert_eq!(masked.matches('\n').count(), 2);
        assert!(!masked.contains("prepare"));
        assert!(!masked.contains("unavailable"));
        assert!(!masked.contains("fn x"));
        assert!(masked.contains("'a: loop {}"));
        assert!(mask("let s = \"é\\\"}\"; let t = 'é';").ends_with("let t = '  ';"));
    }

    #[test]
    fn test_items_are_skipped_without_hiding_later_production_code() {
        let text = "\
#[cfg(test)]
#[path = \"a_tests.rs\"]
mod a_tests;
#[path = \"live.rs\"]
mod live;
struct S {
    #[cfg(test)]
    hook: u8,
    field: u8,
}
#[cfg(test)]
mod tests {
    fn helper() -> Result<(), String> {
        let _ = c.prepare(\"SELECT 1\");
    }
}
fn after() -> Result<(), String> {}
#[cfg(all(test, any(target_os = \"linux\", target_os = \"macos\")))]
#[path = \"b_tests.rs\"]
mod b_tests;
#[cfg(any(test, unix))]
#[path = \"shared.rs\"]
mod shared;
";
        assert_eq!(
            rules("src/a.rs", text),
            [
                (PATH_ATTRIBUTE.to_owned(), 4),
                (PATH_ATTRIBUTE.to_owned(), 22),
                (STRING_ERROR_RESULT.to_owned(), 17)
            ]
        );
    }

    #[test]
    fn string_error_signatures_are_found_across_lines_and_paths() {
        let text = "\
pub(crate) async fn a<T: Fn() -> u8>(
    f: T,
) -> std::result::Result<Vec<(u8, String)>, String>
where
    T: Copy,
{}
fn b() -> Result<String, Error> {}
fn c(f: impl Fn() -> Result<(), String>) {}
fn d() -> io::Result<String> {}
type Alias<T> = Result<T, String>;
let f = || -> Result<(), String> { Ok(()) };
fn e() -> Result<Box<dyn Fn() -> u8>, std::string::String>;
";
        assert_eq!(
            rules("src/a.rs", text),
            [
                (STRING_ERROR_RESULT.to_owned(), 1),
                (STRING_ERROR_RESULT.to_owned(), 12),
                (STRING_ERROR_RESULT.to_owned(), 10)
            ]
        );
    }

    #[test]
    fn helper_attribute_prepare_and_sql_rules_match_only_code() {
        let text = "\
fn unavailable(e: impl Display) -> Error { todo() }
fn unavailable_count() {}
#[allow(clippy::too_many_arguments, reason = \"x\")]
#[expect(clippy::too_many_arguments)]
let s = c.prepare(\"SELECT 1\")?; let t = c.prepare_cached(\"SELECT 1\")?;
let q = format!(\"SELECT {COLUMNS} FROM objects WHERE a = ?1\");
let r = format!(
    r#\"INSERT INTO t
        (a) VALUES (?1)\"#,
);
let u = format!(\"DELETE /upload/{id} HTTP/1.1\");
let v = format!(\"select failed: {e}\");
// format!(\"SELECT a FROM t\")
";
        assert_eq!(
            rules("src/a.rs", text),
            [
                (UNCACHED_PREPARE.to_owned(), 5),
                (UNAVAILABLE_HELPER.to_owned(), 1),
                (FORMAT_SQL.to_owned(), 6),
                (FORMAT_SQL.to_owned(), 7),
                (ALLOW_TOO_MANY_ARGUMENTS.to_owned(), 3),
                (ALLOW_TOO_MANY_ARGUMENTS.to_owned(), 4)
            ]
        );
    }

    #[test]
    fn too_many_arguments_attributes_are_found_across_lines() {
        let text = "\
#[allow(
    clippy::too_many_arguments,
    reason = \"The shared storage contract fixes this signature\"
)]
fn a() {}
    #[expect(clippy::needless_pass_by_value, clippy::too_many_arguments, reason = \"x\")]
#![allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), expect(
    clippy::too_many_arguments
))]
#[allow(clippy::too_many_lines, reason = \"clippy::too_many_arguments is not set here\")]
#[allow(clippy::too_many_arguments_extra)]
#[doc = \"clippy::too_many_arguments\"]
let index = values[clippy::too_many_arguments];
#[allow(
    clippy::too_many_arguments,
    reason = \"test-only helper\"
)]
#[cfg(test)]
fn test_only() {}
";
        assert_eq!(lines_of(ALLOW_TOO_MANY_ARGUMENTS, text), [1, 6, 7, 8]);
    }

    #[test]
    fn uncached_statements_need_a_literal_or_constant_sql_argument() {
        let text = "\
connection.execute(\"DELETE FROM t WHERE a = ?1\", [a])?;
let n = transaction
    .query_row(
        r#\"SELECT count(*) FROM t\"#,
        [],
        |row| row.get(0),
    )?;
tx.execute(&INSERT_OBJECT, params![a])?;
tx.query_one(schema::SELECT_ROW, [], |row| row.get(0))?;
statement.execute(params![a])?;
statement.query_row([a], |row| row.get(0))?;
cached.execute((a, b))?;
tx.execute(&sql, [])?;
tx.execute(SqlKind::Insert, [])?;
connection.execute_batch(SCHEMA)?;
executor.execute(\"not sql\");
fn execute(\"x\") {}
";
        assert_eq!(lines_of(UNCACHED_STATEMENT, text), [1, 8, 16, 3, 9]);
    }

    #[test]
    fn string_error_variants_are_found_only_for_source_names_in_error_enums() {
        let text = "\
pub(crate) enum AdminError {
    /// Backend(String) in a doc comment.
    #[allow(dead_code)]
    Backend(String),
    Invalid(String),
    Unavailable(
        std::string::String,
    ),
    Io(io::Error),
    Internal { message: String },
    Other(String, u8),
    Storage(String) = 3,
}
enum Error { Sqlite(String), Database(Box<str>) }
enum Outcome { Backend(String) }
struct ParseError(String);
";
        assert_eq!(lines_of(STRING_ERROR_VARIANT, text), [4, 6, 12, 14]);
    }

    #[test]
    fn stringified_errors_are_found_when_the_error_is_dropped() {
        let text = "\
.map_err(|error| AdminError::Backend(error.to_string()))?;
.map_err(|e| io::Error::other(format!(\"seed import: {e}\")))?;
.map_err(|e| invalid(
    format!(\"bad value: {}\", e),
))?;
.map_err(|e| lock_failure(format!(\"open lock: {e}\"), e))?;
.map_err(|_| JsonError::invalid_parameter(format!(\"{key} must be an int64\")))?;
.map_err(|e| io::Error::other(e))?;
.map_err(|e| Error::Io(e))?;
.map_err(|e| invalid(format!(\"{key} is invalid\")))?;
.map_err(|e| { log(e); Error::Backend(e.to_string()) })?;
.map_err(Error::Backend)?;
";
        assert_eq!(lines_of(STRINGIFIED_ERROR, text), [1, 2, 3]);
    }

    #[test]
    fn pending_rules_warn_and_cross_target_findings_are_pending() {
        let enforced = Finding::new("clippy::expect_used", "src/a.rs", 1);
        let pending = Finding::new(STRINGIFIED_ERROR, "src/a.rs", 2);
        let all = vec![enforced.clone(), pending.clone()];
        let host = split_pending(all.clone(), false);
        let cross = split_pending(all.clone(), true);
        for rule in PENDING_RULES {
            assert!(
                TEXT_RULES.contains(rule) || CLIPPY_LINTS.contains(rule) || *rule == CROSS_TARGET,
                "{rule} is not a gate rule"
            );
        }
        if PENDING_RULES.contains(&STRINGIFIED_ERROR) {
            assert_eq!(host, (vec![enforced.clone()], vec![pending]));
        } else {
            assert_eq!(host, (all.clone(), Vec::new()));
        }
        if PENDING_RULES.contains(&CROSS_TARGET) {
            assert_eq!(cross, (Vec::new(), all));
        } else {
            assert_eq!(cross.0.first(), Some(&enforced));
        }
    }

    #[test]
    fn file_suppressions_cover_a_whole_file_for_file_scope_rules_only() {
        let text = "\
// xtask-lint: allow-file(text::uncached_statement) -- schema setup runs once at open
fn open(c: &Connection) {
    c.execute(\"CREATE TABLE t (a)\", [])?;
    // xtask-lint: allow(text::uncached_statement) -- a one-off repair at startup
    c.execute(\"DELETE FROM t\", [])?;
    c.query_row(\"SELECT 1\", [], |r| r.get(0))?;
}
";
        let scan = scan_source("src/storage/a.rs", text);
        assert_eq!(scan.problems, []);
        assert!(scan.findings.is_empty(), "{:?}", scan.findings);
        assert_eq!(
            suppression_summary(&scan.suppressed),
            "3 reasoned suppressions (text::uncached_statement: 3)"
        );

        let stale = scan_source(
            "src/a.rs",
            "// xtask-lint: allow-file(text::uncached_statement) -- schema setup runs once\nlet a = 1;\n",
        );
        assert_eq!(
            stale
                .problems
                .iter()
                .map(|p| p.message.as_str())
                .collect::<Vec<_>>(),
            ["stale suppression: `text::uncached_statement` reports nothing on this file"]
        );
        assert!(
            parse_suppression(
                "// xtask-lint: allow-file(text::format_sql) -- a long enough reason"
            )
            .is_some_and(|parsed| parsed.is_err_and(|e| e.contains("whole file")))
        );
        assert_eq!(
            parse_suppression(
                "// xtask-lint: allow-file(text::uncached_statement) -- migrations run once"
            ),
            Some(Ok(ParsedSuppression {
                scope: SuppressionScope::File,
                rules: vec![UNCACHED_STATEMENT.to_owned()]
            }))
        );
    }

    #[test]
    fn file_suppressions_are_rejected_in_request_adapters() {
        let text = "\
// xtask-lint: allow-file(text::uncached_statement) -- request-free module, supposedly
fn handle(c: &Connection) {
    c.execute(\"DELETE FROM t\", [])?;
}
";
        for file in [
            "src/server/upload.rs",
            "src/server.rs",
            "src/json/mod.rs",
            "src/xml/post.rs",
            "src/grpc/v2/objects.rs",
        ] {
            let scan = scan_source(file, text);
            assert_eq!(
                scan.problems
                    .iter()
                    .map(|p| (p.line, p.message.contains("request adapters")))
                    .collect::<Vec<_>>(),
                [(1, true)],
                "{file}"
            );
            // The rejected comment suppresses nothing, so the finding still fails the gate.
            assert_eq!(scan.findings.len(), 1, "{file}");
            assert!(scan.suppressed.is_empty(), "{file}");
        }
        for file in [
            "src/serverless.rs",
            "src/storage/catalog/sync_state/root.rs",
        ] {
            let scan = scan_source(file, text);
            assert_eq!(scan.problems, [], "{file}");
            assert_eq!(scan.suppressed.len(), 1, "{file}");
        }
        let line = "\
fn handle(c: &Connection) {
    // xtask-lint: allow(text::uncached_statement) -- a one-off repair at startup
    c.execute(\"DELETE FROM t\", [])?;
}
";
        let scan = scan_source("src/server/upload.rs", line);
        assert_eq!(scan.problems, []);
        assert_eq!(scan.suppressed.len(), 1);
    }

    #[test]
    fn only_server_production_files_are_scanned() {
        for path in [
            "src/server.rs",
            "src/storage/catalog.rs",
            "src/grpc/v2/mod.rs",
        ] {
            assert!(production_file(path), "{path}");
        }
        for path in [
            "src/grpc/generated/google.storage.v2.rs",
            "src/grpc/codegen/src/main.rs",
            "src/storage/catalog_tests.rs",
            "src/json/tests.rs",
            "src/storage/resumable_http_tests/upload_replay.rs",
            "src/storage/catalog/change_journal/tests/observation.rs",
            "src/test_support.rs",
            "tests/common/mod.rs",
            "xtask/src/lint.rs",
            "src/mime.tsv",
        ] {
            assert!(!production_file(path), "{path}");
        }
    }

    #[test]
    fn clippy_stream_keeps_curated_production_findings_once() {
        let message = |code: &str, file: &str, line: u64| {
            serde_json::json!({
                "reason": "compiler-message",
                "message": {"code": {"code": code}, "level": "warning", "spans": [
                    {"file_name": "src/other.rs", "line_start": 1, "column_start": 1, "is_primary": false},
                    {"file_name": file, "line_start": line, "column_start": 5, "is_primary": true}
                ]}
            })
            .to_string()
        };
        let stream = [
            message("clippy::unwrap_used", "src/server.rs", 10),
            message("clippy::unwrap_used", "src/server.rs", 10),
            message("clippy::unwrap_used", "src\\url.rs", 3),
            message("clippy::needless_return", "src/server.rs", 11),
            message("clippy::panic", "src/grpc/generated/x.rs", 1),
            message("clippy::panic", "src/grpc/v1/../generated/x.rs", 1),
            message("clippy::panic", "src/grpc/./v2/../mod.rs", 2),
            message("clippy::too_many_lines", "src/storage/catalog_tests.rs", 1),
            message("clippy::unwrap_used", "/registry/src/lib.rs", 1),
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
            "not json".to_owned(),
        ]
        .join("\n");
        assert_eq!(
            clippy_findings(&stream, Scope::Production),
            [
                Finding::new("clippy::unwrap_used", "src/server.rs", 10),
                Finding::new("clippy::unwrap_used", "src/url.rs", 3),
                Finding::new("clippy::panic", "src/grpc/mod.rs", 2)
            ]
        );
    }

    #[test]
    fn every_finding_fails_except_one_shared_helper_in_its_file() {
        let helper = |file: &str| scan_source(file, "fn unavailable() {}").findings;
        let unwrap = Finding::new("clippy::unwrap_used", "src/x.rs", 1);

        let shared = helper(SHARED_HELPER_FILE);
        assert_eq!(violations(shared.clone()), (Vec::new(), None));

        let mut copied = shared;
        copied.extend(helper("src/b.rs"));
        copied.push(unwrap.clone());
        assert_eq!(
            violations(copied),
            (
                vec![Finding::new(UNAVAILABLE_HELPER, "src/b.rs", 1), unwrap],
                None
            )
        );

        // Moving the helper elsewhere both reports the copy and the missing shared one.
        let (moved, missing) = violations(helper("src/c.rs"));
        assert_eq!(moved, [Finding::new(UNAVAILABLE_HELPER, "src/c.rs", 1)]);
        assert!(missing.unwrap().contains(SHARED_HELPER_FILE));

        // A second definition in the shared file reports both.
        let twice = scan_source(
            SHARED_HELPER_FILE,
            "fn unavailable() {}\nmod m { fn unavailable() {} }\n",
        )
        .findings;
        assert_eq!(violations(twice).0.len(), 2);
    }

    #[test]
    fn arguments_select_one_mode() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(mode(&args(&[])), Some(Mode::Gate));
        assert_eq!(
            mode(&args(&["--all-targets-report"])),
            Some(Mode::AllTargetsReport)
        );
        assert_eq!(
            mode(&args(&["--target", "x86_64-pc-windows-msvc"])),
            Some(Mode::Target("x86_64-pc-windows-msvc".to_owned()))
        );
        for bad in [
            &["--strict"][..],
            &["--update-baseline"],
            &["--all-targets-report", "x"],
            &["x"],
            &["--target"],
            &["--target", ""],
            &["--target", "--all-targets-report"],
            &["--target", "a", "--target", "b"],
        ] {
            assert_eq!(mode(&args(bad)), None);
        }
    }

    fn lines_of(rule: &str, text: &str) -> Vec<usize> {
        rules("src/a.rs", text)
            .into_iter()
            .filter(|(r, _)| r == rule)
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn syncing_a_written_tokio_file_without_flush_is_reported() {
        let text = "\
async fn unflushed(mut file: tokio::fs::File) -> io::Result<()> {
    file.write_all(b\"x\").await?;
    file.sync_all().await
}
async fn flushed(mut file: tokio::fs::File) -> io::Result<()> {
    file.write_all(b\"x\").await?;
    file.flush().await?;
    file.sync_all().await
}
async fn written_after_flush(mut file: tokio::fs::File) -> io::Result<()> {
    file.write_all(b\"x\").await?;
    file.flush().await?;
    file
        .write_buf(&mut chunk)
        .await?;
    file.sync_data()
        .await
}
async fn shut_down(mut file: tokio::fs::File) -> io::Result<()> {
    file.write(&buf).await?;
    file.shutdown().await?;
    file.sync_all().await
}
async fn read_only(mut file: tokio::fs::File) -> io::Result<()> {
    file.read(&mut buf).await?;
    file.sync_all().await
}
fn std_file(mut file: std::fs::File) -> io::Result<()> {
    file.write_all(b\"x\")?;
    file.sync_all()
}
async fn lock_guard(state: &tokio::sync::RwLock<u8>, file: tokio::fs::File) -> io::Result<()> {
    let _guard = state.write().await;
    let _open = OpenOptions::new().write(true).open(path).await?;
    file.sync_all().await
}
async fn in_task(mut output: tokio::fs::File) {
    tokio::spawn(async move {
        output.write_all(&buffer[..n]).await?;
        // output.flush().await
        output.sync_all().await
    });
}
async fn next_function_starts_clean(file: tokio::fs::File) -> io::Result<()> {
    file.sync_all().await
}
#[cfg(test)]
async fn test_helper(mut file: tokio::fs::File) {
    file.write_all(b\"x\").await.unwrap();
    file.sync_all().await.unwrap();
}
";
        assert_eq!(lines_of(SYNC_WITHOUT_FLUSH, text), [3, 16, 41]);
    }

    #[test]
    fn reasoned_suppressions_silence_findings_and_are_counted() {
        let text = "\
let a = format!(\"SELECT {C} FROM t\"); // xtask-lint: allow(text::format_sql) -- static column list
// xtask-lint: allow(text::format_sql) -- schema migration builds DDL
// from a fixed table of names.
let b = format!(\"CREATE TABLE {name} (a)\");
// xtask-lint: allow(text::uncached_prepare, text::format_sql) -- one-off startup migration
let c = conn.prepare(&format!(\"DELETE FROM {t} WHERE a = 1\"));
let d = format!(\"SELECT {C} FROM t\");
";
        let scan = scan_source("src/a.rs", text);
        assert_eq!(scan.problems, []);
        assert_eq!(
            scan.findings,
            [Finding::new(FORMAT_SQL, "src/a.rs", 7)],
            "only the unsuppressed statement counts"
        );
        assert_eq!(
            scan.suppressed
                .iter()
                .map(|f| (f.rule.as_str(), f.line))
                .collect::<Vec<_>>(),
            [
                (UNCACHED_PREPARE, 6),
                (FORMAT_SQL, 1),
                (FORMAT_SQL, 4),
                (FORMAT_SQL, 6)
            ]
        );
        assert_eq!(
            suppression_summary(&scan.suppressed),
            "4 reasoned suppressions (text::format_sql: 3, text::uncached_prepare: 1)"
        );
        assert_eq!(suppression_summary(&[]), "0 reasoned suppressions");
    }

    #[test]
    fn invalid_and_stale_suppressions_are_errors() {
        let problems = |text: &str| -> Vec<(usize, String)> {
            scan_source("src/a.rs", text)
                .problems
                .into_iter()
                .map(|p| (p.line, p.message))
                .collect()
        };
        let sql = "let q = format!(\"SELECT a FROM t\");";
        let single = |comment: &str| {
            let found = problems(&format!("{comment}\n{sql}\n"));
            assert_eq!(found.len(), 1, "{comment}: {found:?}");
            found[0].1.clone()
        };
        assert!(single("// xtask-lint: allow(text::format_sql)").contains("reason of at least 10"));
        assert!(single("// xtask-lint: allow(text::format_sql) --").contains("reason"));
        assert!(single("// xtask-lint: allow(text::format_sql) -- legacy").contains("reason"));
        assert!(
            single("// xtask-lint: allow(text::nope) -- a long enough reason")
                .contains("unknown rule `text::nope`")
        );
        assert!(
            single("// xtask-lint: allow(clippy::unwrap_used) -- a long enough reason")
                .contains("#[expect(clippy::unwrap_used")
        );
        assert!(
            single("// xtask-lint: allow(unreachable_pub) -- a long enough reason")
                .contains("#[expect(")
        );
        for malformed in [
            "// xtask-lint: text::format_sql -- a long enough reason",
            "// xtask-lint: allow(text::format_sql -- a long enough reason",
            "// xtask-lint: allow() -- a long enough reason",
            "// xtask-lint: allow(text::format_sql) because it is a long reason",
        ] {
            assert!(single(malformed).contains("malformed"), "{malformed}");
        }
        // A rejected suppression suppresses nothing.
        assert_eq!(
            scan_source(
                "src/a.rs",
                &format!("// xtask-lint: allow(text::format_sql)\n{sql}\n")
            )
            .findings
            .len(),
            1
        );
        // Stale: nothing to suppress, a different rule, or only one of two rules used.
        assert_eq!(
            problems("let a = 1; // xtask-lint: allow(text::format_sql) -- static column list\n"),
            [(
                1,
                "stale suppression: `text::format_sql` reports nothing on the line it covers"
                    .to_owned()
            )]
        );
        assert_eq!(
            problems(&format!(
                "// xtask-lint: allow(text::uncached_prepare, text::format_sql) -- static column list\n{sql}\n"
            ))
            .len(),
            1
        );
        // A suppression covers one line, not the rest of the function.
        assert_eq!(
            problems(&format!(
                "// xtask-lint: allow(text::format_sql) -- static column list\nlet x = 1;\n{sql}\n"
            ))
            .len(),
            1
        );
    }

    #[test]
    fn suppression_text_in_doc_comments_and_strings_is_ignored() {
        let text = "\
/// // xtask-lint: allow(text::format_sql)
//! xtask-lint: allow(nothing)
let s = \"// xtask-lint: allow(text::nope)\";
// an ordinary comment mentioning xtask-lint: allow(text::format_sql)
";
        let scan = scan_source("src/a.rs", text);
        assert_eq!(scan.problems, []);
        assert_eq!(parse_suppression("// ordinary"), None);
        assert_eq!(
            parse_suppression("//xtask-lint: allow( text::format_sql ) --   ten chars!  "),
            Some(Ok(ParsedSuppression {
                scope: SuppressionScope::Line,
                rules: vec![FORMAT_SQL.to_owned()]
            }))
        );
    }

    #[test]
    fn workspace_scope_keeps_every_repository_file() {
        let message = |file: &str| {
            serde_json::json!({
                "reason": "compiler-message",
                "message": {"code": {"code": "clippy::unwrap_used"}, "level": "warning", "spans": [
                    {"file_name": file, "line_start": 1, "column_start": 1, "is_primary": true}
                ]}
            })
            .to_string()
        };
        let stream = [
            "src/server.rs",
            "tests/common/mod.rs",
            "xtask/src/main.rs",
            "/registry/src/lib.rs",
            "C:\\registry\\src\\lib.rs",
            "../outside/lib.rs",
        ]
        .map(message)
        .join("\n");
        assert_eq!(
            clippy_findings(&stream, Scope::Workspace)
                .into_iter()
                .map(|f| f.file)
                .collect::<Vec<_>>(),
            ["src/server.rs", "tests/common/mod.rs", "xtask/src/main.rs"]
        );
        assert_eq!(clippy_findings(&stream, Scope::Production).len(), 1);
    }

    #[test]
    fn all_targets_report_counts_per_file_and_lists_clean_lints() {
        let workspace = [
            Finding::new("clippy::expect_used", "src/a.rs", 3),
            Finding::new("clippy::too_many_lines", "tests/x.rs", 1),
            Finding::new("clippy::too_many_lines", "tests/x.rs", 90),
        ];
        assert_eq!(
            counts_by_file(&workspace).into_iter().collect::<Vec<_>>(),
            [
                (("clippy::expect_used".to_owned(), "src/a.rs".to_owned()), 1),
                (
                    ("clippy::too_many_lines".to_owned(), "tests/x.rs".to_owned()),
                    2
                )
            ]
        );
        let ready = ready_lints(&workspace);
        assert!(!ready.contains(&"clippy::expect_used"));
        assert!(!ready.contains(&"clippy::too_many_lines"));
        assert!(ready.contains(&"clippy::unwrap_used"));
        assert_eq!(ready.len(), CLIPPY_LINTS.len() - 2);
    }

    #[test]
    fn workspace_denied_lints_are_not_downgraded_by_the_gate() {
        // `-W` on the command line would override a `deny` from `[workspace.lints]`. An
        // `allow` there is what the gate deliberately raises for production code only.
        let mut section = "";
        let mut denied = Vec::new();
        for line in include_str!("../../Cargo.toml").lines() {
            if line.starts_with('[') {
                section = line;
            } else if section.starts_with("[workspace.lints")
                && let Some((name, level)) = line.split_once(" = ")
                && level != "\"allow\""
            {
                denied.push(name);
            }
        }
        assert!(denied.contains(&"unreachable_pub"), "{denied:?}");
        for lint in CLIPPY_LINTS {
            let name = lint.trim_start_matches("clippy::");
            assert!(
                !denied.contains(&name),
                "{lint} is set in [workspace.lints]; remove it from CLIPPY_LINTS"
            );
        }
    }
}
