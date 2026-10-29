//! Ratcheting code-quality gate for the server crate.
//!
//! Existing violations are recorded per rule and file in the baseline. A check fails when a
//! count grows, so new violations cannot land, and also when a count shrinks, so the baseline
//! always tracks the tree and a fix cannot be silently undone later. `--strict` ignores the
//! baseline and is the end state once every recorded violation is fixed.

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::Path,
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, String>;
/// Rule name, then file (or [`ALL_FILES`] for a tree-wide rule), then occurrence count.
pub type Counts = BTreeMap<String, BTreeMap<String, u64>>;

pub const BASELINE: &str = "xtask/lint-baseline.json";
const ALL_FILES: &str = "(all files)";
const UPDATE: &str = "cargo xtask lint --update-baseline";

/// Extra Clippy and rustc lints counted on production code. Thresholds live in `clippy.toml`.
pub const CLIPPY_LINTS: [&str; 9] = [
    "clippy::unwrap_used",
    "clippy::expect_used",
    "clippy::panic",
    "clippy::too_many_lines",
    "clippy::fn_params_excessive_bools",
    "clippy::needless_pass_by_value",
    "clippy::redundant_clone",
    "clippy::allow_attributes_without_reason",
    "unreachable_pub",
];

const PATH_ATTRIBUTE: &str = "text::path_attribute";
const STRING_ERROR_RESULT: &str = "text::string_error_result";
const UNAVAILABLE_HELPER: &str = "text::unavailable_helper";
const ALLOW_TOO_MANY_ARGUMENTS: &str = "text::allow_too_many_arguments";
const UNCACHED_PREPARE: &str = "text::uncached_prepare";
const FORMAT_SQL: &str = "text::format_sql";

const SQL_KEYWORDS: [&str; 10] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "REPLACE", "WITH", "CREATE", "DROP", "ALTER", "PRAGMA",
];
const SQL_MARKERS: [&str; 7] = [
    " FROM ", " INTO ", " SET ", " TABLE ", " INDEX ", " VALUES", "PRAGMA ",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Fail on any count above or below the baseline.
    Ratchet,
    /// Ignore the baseline; fail on any violation.
    Strict,
    /// Rewrite the baseline; growth needs [`Growth::Allow`].
    Update(Growth),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Growth {
    Refuse,
    Allow,
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

    fn key(&self) -> &str {
        if self.rule == UNAVAILABLE_HELPER {
            ALL_FILES
        } else {
            &self.file
        }
    }
}

/// Counts that differ from the reference: `(rule, file, reference, current)`.
pub type Changes = Vec<(String, String, u64, u64)>;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    pub increased: Changes,
    pub decreased: Changes,
}

/// Parses the arguments after `lint`.
pub fn mode(args: &[String]) -> Option<Mode> {
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => Some(Mode::Ratchet),
        ["--strict"] => Some(Mode::Strict),
        ["--update-baseline"] => Some(Mode::Update(Growth::Refuse)),
        ["--update-baseline", "--allow-increase"] | ["--allow-increase", "--update-baseline"] => {
            Some(Mode::Update(Growth::Allow))
        }
        _ => None,
    }
}

/// Runs the gate and returns whether it passed.
pub fn run(root: &Path, mode: Mode) -> Result<bool> {
    let path = root.join(BASELINE);
    // Read the baseline before the slow Clippy run so a missing or malformed file fails fast.
    let baseline = match (fs::read_to_string(&path), mode) {
        (_, Mode::Strict) => Counts::new(),
        (Ok(text), _) => parse_baseline(&text)?,
        (Err(e), Mode::Update(_)) if e.kind() == std::io::ErrorKind::NotFound => Counts::new(),
        (Err(e), _) => {
            return Err(format!(
                "{BASELINE}: {e}; create it with `{UPDATE} --allow-increase`"
            ));
        }
    };
    let mut findings = clippy(root)?;
    findings.extend(scan_tree(root)?);
    findings.sort();
    let current = count(&findings);
    match mode {
        Mode::Strict => {
            let failures = strict_failures(&current);
            report(&failures, &findings, "strict lint violation");
            Ok(failures.is_empty())
        }
        Mode::Ratchet => {
            let comparison = compare(&baseline, &current);
            report(&comparison.increased, &findings, "new lint violation");
            for (rule, file, base, now) in &comparison.decreased {
                eprintln!("error: stale lint baseline: {rule} in {file}: {now} (baseline {base})");
            }
            if !comparison.increased.is_empty() {
                eprintln!(
                    "Fix the violations listed above. Accepting them is a deliberate, reviewed act: `{UPDATE} --allow-increase`."
                );
            }
            if !comparison.decreased.is_empty() {
                eprintln!(
                    "Violations were fixed; record the lower counts with `{UPDATE}` and commit {BASELINE}."
                );
            }
            let passed = comparison == Comparison::default();
            if passed {
                println!(
                    "lint ratchet: {} recorded violations, none new",
                    total(&current)
                );
            }
            Ok(passed)
        }
        Mode::Update(growth) => {
            let comparison = compare(&baseline, &current);
            if growth == Growth::Refuse && !comparison.increased.is_empty() {
                report(
                    &comparison.increased,
                    &findings,
                    "refusing to raise the baseline",
                );
                eprintln!(
                    "Fix these violations, or pass --allow-increase to record them deliberately."
                );
                return Ok(false);
            }
            fs::write(&path, render_baseline(&current)?).map_err(|e| format!("{BASELINE}: {e}"))?;
            println!(
                "wrote {BASELINE}: {} violations ({} lowered, {} raised)",
                total(&current),
                comparison.decreased.len(),
                comparison.increased.len()
            );
            Ok(true)
        }
    }
}

fn total(counts: &Counts) -> u64 {
    counts.values().flat_map(BTreeMap::values).sum()
}

fn report(changes: &Changes, findings: &[Finding], heading: &str) {
    for (rule, file, base, now) in changes {
        eprintln!("error: {heading}: {rule} in {file}: {now} (allowed {base})");
        for finding in findings
            .iter()
            .filter(|f| &f.rule == rule && f.key() == file)
        {
            eprintln!("  {}:{}", finding.file, finding.line);
        }
    }
}

fn clippy(root: &Path) -> Result<Vec<Finding>> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    // Only the server's library and binary: `--lib --bins` leaves `cfg(test)` code uncompiled,
    // so test helpers never count. xtask and the gRPC code generator are build tooling.
    command.current_dir(root).args([
        "clippy",
        "--package",
        "gcs-mimic",
        "--lib",
        "--bins",
        "--locked",
        "--offline",
        "--message-format=json",
        "--",
    ]);
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
    Ok(clippy_findings(&stdout))
}

/// Extracts curated lint findings on production files from a `--message-format=json` stream.
/// The library and binary targets can report the same span, so findings are de-duplicated.
pub fn clippy_findings(stream: &str) -> Vec<Finding> {
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
        let file = normalize(span["file_name"].as_str().unwrap_or_default());
        let line = span["line_start"].as_u64().unwrap_or_default();
        let column = span["column_start"].as_u64().unwrap_or_default();
        if !production_file(&file) || !seen.insert((code.to_owned(), file.clone(), line, column)) {
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
    let name = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("src/")
        && path.ends_with(".rs")
        && !path.starts_with("src/grpc/generated/")
        && !path.starts_with("src/grpc/codegen/")
        && !name.ends_with("_tests.rs")
        && name != "tests.rs"
        && name != "test_support.rs"
}

fn scan_tree(root: &Path) -> Result<Vec<Finding>> {
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
    let mut findings = Vec::new();
    for (relative, path) in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?;
        findings.extend(scan_source(&relative, &text));
    }
    Ok(findings)
}

/// Applies the text rules to one production file, skipping `#[cfg(test)]` items.
pub fn scan_source(file: &str, text: &str) -> Vec<Finding> {
    let masked = mask(text);
    let tests = test_lines(&masked);
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
        let attribute = ["#[allow(", "#[expect(", "#![allow(", "#![expect("]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix));
        if attribute && trimmed.contains("clippy::too_many_arguments") {
            findings.push(Finding::new(ALLOW_TOO_MANY_ARGUMENTS, file, index + 1));
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
    for offset in words(&masked, "fn") {
        if let Some((name, returns)) = signature(&masked, offset) {
            if name == "unavailable" {
                add(UNAVAILABLE_HELPER, offset);
            }
            if returns.is_some_and(string_error_result) {
                add(STRING_ERROR_RESULT, offset);
            }
        }
    }
    for offset in words(&masked, "type") {
        if alias(&masked, offset).is_some_and(string_error_result) {
            add(STRING_ERROR_RESULT, offset);
        }
    }
    for offset in words(&masked, "format") {
        if format_sql(text, &masked, offset) {
            add(FORMAT_SQL, offset);
        }
    }
    findings
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
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
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
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether an attribute compiles its item only under test: `cfg(test)` or `cfg(all(.., test, ..))`.
fn test_only(attribute: &str) -> bool {
    attribute == "#[cfg(test)]"
        || attribute
            .strip_prefix("#[cfg(all(")
            .and_then(|rest| rest.strip_suffix("))]"))
            .is_some_and(|items| items.split(',').any(|item| item.trim() == "test"))
}

/// Marks the lines of each test-only item, from the attribute through its balanced end.
pub fn test_lines(masked: &str) -> Vec<bool> {
    let lines: Vec<&str> = masked.lines().collect();
    let mut tests = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if !test_only(lines[i].trim()) {
            i += 1;
            continue;
        }
        let start = i;
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

pub fn count(findings: &[Finding]) -> Counts {
    let mut counts = Counts::new();
    for finding in findings {
        *counts
            .entry(finding.rule.clone())
            .or_default()
            .entry(finding.key().to_owned())
            .or_default() += 1;
    }
    counts
}

pub fn compare(reference: &Counts, current: &Counts) -> Comparison {
    let mut keys = BTreeSet::new();
    for counts in [reference, current] {
        for (rule, files) in counts {
            keys.extend(files.keys().map(|file| (rule.clone(), file.clone())));
        }
    }
    let get = |counts: &Counts, rule: &str, file: &str| {
        counts
            .get(rule)
            .and_then(|files| files.get(file))
            .copied()
            .unwrap_or(0)
    };
    let mut comparison = Comparison::default();
    for (rule, file) in keys {
        let (before, after) = (get(reference, &rule, &file), get(current, &rule, &file));
        if after > before {
            comparison.increased.push((rule, file, before, after));
        } else if after < before {
            comparison.decreased.push((rule, file, before, after));
        }
    }
    comparison
}

/// Violations of the final policy: none at all, except the one shared error-mapping helper.
pub fn strict_failures(current: &Counts) -> Changes {
    let mut failures = Changes::new();
    for (rule, files) in current {
        let limit = u64::from(rule == UNAVAILABLE_HELPER);
        for (file, &n) in files {
            if n > limit {
                failures.push((rule.clone(), file.clone(), limit, n));
            }
        }
    }
    failures
}

pub fn parse_baseline(text: &str) -> Result<Counts> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("{BASELINE}: {e}"))?;
    if value["schema_version"] != 1 {
        return Err(format!("{BASELINE}: unsupported schema_version"));
    }
    let mut counts = Counts::new();
    for (rule, files) in value["counts"]
        .as_object()
        .ok_or_else(|| format!("{BASELINE}: counts must be an object"))?
    {
        let files = files
            .as_object()
            .ok_or_else(|| format!("{BASELINE}: {rule} must map files to counts"))?;
        for (file, n) in files {
            let n = n.as_u64().filter(|&n| n > 0).ok_or_else(|| {
                format!("{BASELINE}: {rule} in {file}: expected a positive count")
            })?;
            counts
                .entry(rule.clone())
                .or_default()
                .insert(file.clone(), n);
        }
    }
    Ok(counts)
}

/// Stable, sorted JSON so baseline diffs show exactly which counts moved.
pub fn render_baseline(counts: &Counts) -> Result<String> {
    // Serializing the map itself (not a `Value`) keeps `BTreeMap` order whatever serde_json
    // features the workspace enables.
    let body = serde_json::to_string_pretty(counts).map_err(|e| e.to_string())?;
    Ok(format!(
        "{{\n  \"schema_version\": 1,\n  \"counts\": {}\n}}\n",
        body.replace('\n', "\n  ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(file: &str, text: &str) -> Vec<(String, usize)> {
        scan_source(file, text)
            .into_iter()
            .map(|f| (f.rule, f.line))
            .collect()
    }

    fn counts(entries: &[(&str, &str, u64)]) -> Counts {
        let mut counts = Counts::new();
        for (rule, file, n) in entries {
            counts
                .entry((*rule).to_owned())
                .or_default()
                .insert((*file).to_owned(), *n);
        }
        counts
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
                (ALLOW_TOO_MANY_ARGUMENTS.to_owned(), 3),
                (ALLOW_TOO_MANY_ARGUMENTS.to_owned(), 4),
                (UNCACHED_PREPARE.to_owned(), 5),
                (UNAVAILABLE_HELPER.to_owned(), 1),
                (FORMAT_SQL.to_owned(), 6),
                (FORMAT_SQL.to_owned(), 7)
            ]
        );
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
            message("unreachable_pub", "src/grpc/generated/x.rs", 1),
            message("unreachable_pub", "src/grpc/v1/../generated/x.rs", 1),
            message("unreachable_pub", "src/grpc/./v2/../mod.rs", 2),
            message("clippy::too_many_lines", "src/storage/catalog_tests.rs", 1),
            message("clippy::unwrap_used", "/registry/src/lib.rs", 1),
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
            "not json".to_owned(),
        ]
        .join("\n");
        assert_eq!(
            clippy_findings(&stream),
            [
                Finding::new("clippy::unwrap_used", "src/server.rs", 10),
                Finding::new("clippy::unwrap_used", "src/url.rs", 3),
                Finding::new("unreachable_pub", "src/grpc/mod.rs", 2)
            ]
        );
    }

    #[test]
    fn comparison_reports_growth_and_stale_entries() {
        let baseline = counts(&[
            ("a", "src/x.rs", 2),
            ("a", "src/y.rs", 1),
            ("b", "src/x.rs", 1),
        ]);
        let current = counts(&[
            ("a", "src/x.rs", 3),
            ("b", "src/x.rs", 1),
            ("c", "src/z.rs", 1),
        ]);
        let comparison = compare(&baseline, &current);
        assert_eq!(
            comparison.increased,
            [
                ("a".to_owned(), "src/x.rs".to_owned(), 2, 3),
                ("c".to_owned(), "src/z.rs".to_owned(), 0, 1)
            ]
        );
        assert_eq!(
            comparison.decreased,
            [("a".to_owned(), "src/y.rs".to_owned(), 1, 0)]
        );
        assert_eq!(compare(&current, &current), Comparison::default());
    }

    #[test]
    fn the_shared_helper_is_counted_tree_wide_and_strict_allows_one() {
        let one = scan_source("src/a.rs", "fn unavailable() {}");
        let mut two = one.clone();
        two.extend(scan_source("src/b.rs", "fn unavailable() {}"));
        assert_eq!(count(&one), counts(&[(UNAVAILABLE_HELPER, ALL_FILES, 1)]));
        // Moving the helper to another file is not growth.
        let moved = count(&scan_source("src/c.rs", "fn unavailable() {}"));
        assert_eq!(compare(&count(&one), &moved), Comparison::default());
        assert!(strict_failures(&count(&one)).is_empty());
        assert_eq!(
            strict_failures(&count(&two)),
            [(UNAVAILABLE_HELPER.to_owned(), ALL_FILES.to_owned(), 1, 2)]
        );
        assert_eq!(
            strict_failures(&counts(&[("clippy::unwrap_used", "src/x.rs", 1)])).len(),
            1
        );
    }

    #[test]
    fn baseline_round_trips_sorted_and_rejects_malformed_counts() {
        let original = counts(&[
            ("b", "src/y.rs", 2),
            ("a", "src/z.rs", 1),
            ("a", "src/x.rs", 3),
        ]);
        let text = render_baseline(&original).unwrap();
        assert!(text.ends_with("}\n"));
        assert!(text.find("\"a\"").unwrap() < text.find("\"b\"").unwrap());
        assert!(text.find("src/x.rs").unwrap() < text.find("src/z.rs").unwrap());
        assert_eq!(parse_baseline(&text).unwrap(), original);
        assert_eq!(
            render_baseline(&Counts::new()).unwrap(),
            "{\n  \"schema_version\": 1,\n  \"counts\": {}\n}\n"
        );
        for bad in [
            r#"{"schema_version": 2, "counts": {}}"#,
            r#"{"schema_version": 1, "counts": []}"#,
            r#"{"schema_version": 1, "counts": {"a": {"src/x.rs": 0}}}"#,
            r#"{"schema_version": 1, "counts": {"a": {"src/x.rs": -1}}}"#,
        ] {
            assert!(parse_baseline(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn arguments_select_one_mode() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(mode(&args(&[])), Some(Mode::Ratchet));
        assert_eq!(mode(&args(&["--strict"])), Some(Mode::Strict));
        assert_eq!(
            mode(&args(&["--update-baseline"])),
            Some(Mode::Update(Growth::Refuse))
        );
        assert_eq!(
            mode(&args(&["--update-baseline", "--allow-increase"])),
            Some(Mode::Update(Growth::Allow))
        );
        for bad in [
            &["--allow-increase"][..],
            &["--strict", "--update-baseline"],
            &["x"],
        ] {
            assert_eq!(mode(&args(bad)), None);
        }
    }
}
