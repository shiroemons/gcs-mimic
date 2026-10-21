use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use xtask::{read_json, render, run, validate};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

fn ledger() -> Value {
    json!({
        "schema_version": 1, "as_of": "2026-10-21",
        "counts_by_kind": {"api_operation": 0, "behavior": 0, "project_requirement": 1},
        "api_counts_by_category": {},
        "features": [{
            "id": "TEST-001", "kind": "project_requirement", "category": "Governance",
            "title": "Test requirement", "phase": "P0", "status": "NOT_STARTED",
            "target_fidelity": "PROJECT", "actual_fidelity": "NONE",
            "source_urls": [], "source_checked_at": "2026-10-21",
            "source_evidence": "PROPOSED_PROJECT_REQUIREMENT", "dependencies": [],
            "acceptance_status": "NEEDS_DECOMPOSITION", "acceptance_cases": [],
            "test_ids": [], "verification": [], "sdk_evidence": [], "known_deviations": [],
            "last_verified": null
        }]
    })
}

fn completed() -> Value {
    let mut d = ledger();
    let r = &mut d["features"][0];
    r["status"] = json!("IMPLEMENTED");
    r["actual_fidelity"] = json!("PROJECT");
    r["acceptance_status"] = json!("COMPLETE");
    r["test_ids"] = json!(["contract"]);
    r["acceptance_cases"] = json!([{
        "id": "TEST-001-A", "description": "Validate the contract", "result": "PASS", "test_ids": ["contract"]
    }]);
    r["verification"] = json!([{
        "command": "cargo test --locked --offline", "result": "PASS", "verified_at": "2026-10-21",
        "artifact": "README.md", "test_ids": ["contract"]
    }]);
    r["last_verified"] = json!("2026-10-21");
    d
}

#[test]
fn baseline_and_completed_evidence_validate() {
    validate(&root(), &ledger()).unwrap();
    validate(&root(), &completed()).unwrap();
    let actual = read_json(&root().join("compatibility/features.json")).unwrap();
    validate(&root(), &actual).unwrap();
}

#[test]
fn invalid_identity_enums_counts_and_provenance_fail() {
    for (pointer, value, expected) in [
        ("/features/0/id", json!("../bad"), "invalid ID"),
        ("/features/0/status", json!("DONE"), "invalid status"),
        ("/features/0/phase", json!("P8"), "invalid phase"),
        ("/features/0/kind", json!("method"), "invalid kind"),
        (
            "/features/0/actual_fidelity",
            json!("FAST"),
            "invalid actual fidelity",
        ),
        ("/features/0/kind", json!("api_operation"), "kind/target"),
        (
            "/features/0/source_checked_at",
            json!("2026-02-29"),
            "source date",
        ),
        (
            "/features/0/source_evidence",
            json!("UNKNOWN"),
            "source provenance",
        ),
        (
            "/features/0/source_urls",
            json!(["http://example.invalid"]),
            "HTTPS",
        ),
        (
            "/features/0/dependencies",
            json!("TEST-001"),
            "expected array",
        ),
        (
            "/counts_by_kind/project_requirement",
            json!(2),
            "counts_by_kind",
        ),
        ("/schema_version", json!(2), "schema_version"),
    ] {
        let mut d = ledger();
        *d.pointer_mut(pointer).unwrap() = value;
        let error = validate(&root(), &d).unwrap_err();
        assert!(error.contains(expected), "{pointer}: {error}");
    }
    let mut d = ledger();
    let clone = d["features"][0].clone();
    d["features"].as_array_mut().unwrap().push(clone);
    assert!(
        validate(&root(), &d)
            .unwrap_err()
            .contains("duplicate feature ID")
    );
}

#[test]
fn unknown_cyclic_and_incomplete_dependencies_fail() {
    let mut d = ledger();
    d["features"][0]["dependencies"] = json!(["ABSENT-001"]);
    assert!(
        validate(&root(), &d)
            .unwrap_err()
            .contains("unknown dependency")
    );
    d["features"][0]["dependencies"] = json!(["TEST-001"]);
    assert!(validate(&root(), &d).unwrap_err().contains("cycle"));
    let mut second = d["features"][0].clone();
    second["id"] = json!("TEST-002");
    d["features"][0]["dependencies"] = json!(["TEST-002"]);
    d["features"].as_array_mut().unwrap().push(second);
    d["counts_by_kind"]["project_requirement"] = json!(2);
    assert!(validate(&root(), &d).unwrap_err().contains("cycle"));
    d["features"][1]["dependencies"] = json!([]);
    validate(&root(), &d).unwrap();
    d["features"][0] = completed()["features"][0].clone();
    d["features"][0]["dependencies"] = json!(["TEST-002"]);
    assert!(
        validate(&root(), &d)
            .unwrap_err()
            .contains("incomplete dependency")
    );
}

#[test]
fn completed_claims_require_linked_passing_evidence() {
    for (pointer, value, expected) in [
        ("/features/0/acceptance_cases", json!([]), "acceptance"),
        ("/features/0/test_ids", json!([]), "test IDs"),
        ("/features/0/verification", json!([]), "verification"),
        (
            "/features/0/acceptance_status",
            json!("NEEDS_DECOMPOSITION"),
            "COMPLETE",
        ),
        (
            "/features/0/last_verified",
            json!("2026-13-01"),
            "last_verified",
        ),
        (
            "/features/0/actual_fidelity",
            json!("NONE"),
            "target fidelity",
        ),
        (
            "/features/0/known_deviations",
            json!(["Not done"]),
            "unresolved deviations",
        ),
        (
            "/features/0/acceptance_cases/0/result",
            json!("PENDING"),
            "unpassed",
        ),
        (
            "/features/0/acceptance_cases/0/test_ids",
            json!([]),
            "passing evidence",
        ),
        (
            "/features/0/verification/0/result",
            json!("FAIL"),
            "failed verification",
        ),
        (
            "/features/0/verification/0/test_ids",
            json!(["invented"]),
            "unknown/empty",
        ),
        (
            "/features/0/verification/0/artifact",
            json!("docs/missing.txt"),
            "missing.txt",
        ),
        (
            "/features/0/verification/0/artifact",
            json!("../README.md"),
            "traversal",
        ),
        (
            "/features/0/verification/0/artifact",
            json!("/etc/passwd"),
            "traversal",
        ),
    ] {
        let mut d = completed();
        *d.pointer_mut(pointer).unwrap() = value;
        let error = validate(&root(), &d).unwrap_err();
        assert!(error.contains(expected), "{pointer}: {error}");
    }
}

#[test]
fn partial_and_simulated_work_remain_explicit() {
    let mut d = completed();
    d["features"][0]["status"] = json!("PARTIAL");
    assert!(validate(&root(), &d).unwrap_err().contains("missing cases"));
    d["features"][0]["known_deviations"] = json!(["Pending boundary case"]);
    validate(&root(), &d).unwrap();
    d["features"][0]["kind"] = json!("behavior");
    d["features"][0]["target_fidelity"] = json!("LOCAL_SIMULATION");
    d["features"][0]["actual_fidelity"] = json!("EXACT");
    assert!(validate(&root(), &d).unwrap_err().contains("simulation"));
}

#[test]
fn rendering_is_order_independent_and_escapes_markup() {
    let mut d = ledger();
    d["features"][0]["title"] = json!("a | b\n<script>`x` & [link]");
    let rendered = render(&d, None).unwrap();
    assert!(rendered.contains("a &#124; b &lt;script&gt;&#96;x&#96; &amp; &#91;link&#93;"));
    assert!(rendered.contains("| project_requirement | 1 |"));
    let mut second = d["features"][0].clone();
    second["id"] = json!("TEST-002");
    d["features"].as_array_mut().unwrap().push(second);
    let before = render(&d, None).unwrap();
    d["features"].as_array_mut().unwrap().reverse();
    assert_eq!(before, render(&d, None).unwrap());
    let translated = render(
        &d,
        Some(&json!({"titles": {
            "TEST-001": "日本語の補助題名", "TEST-002": "別の補助題名"
        }})),
    )
    .unwrap();
    let status_columns = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| line.starts_with("| TEST-"))
            .map(|line| {
                line.split('|')
                    .enumerate()
                    .filter_map(|(i, cell)| (i != 2).then_some(cell))
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect()
    };
    assert_eq!(status_columns(&before), status_columns(&translated));
    assert!(
        render(&d, Some(&json!({"titles": {}})))
            .unwrap_err()
            .contains("missing companion")
    );
}

#[test]
fn coverage_dimensions_remain_separate() {
    let mut d = completed();
    for (kind, id) in [("api_operation", "API-001"), ("behavior", "BEHAVIOR-001")] {
        let mut row = ledger()["features"][0].clone();
        row["id"] = json!(id);
        row["kind"] = json!(kind);
        row["target_fidelity"] = json!("EXACT");
        row["source_urls"] = json!(["https://docs.cloud.google.com/storage/docs/json_api/v1"]);
        d["features"].as_array_mut().unwrap().push(row);
        d["counts_by_kind"][kind] = json!(1);
    }
    d["api_counts_by_category"] = json!({"Governance": 1});
    validate(&root(), &d).unwrap();
    let text = render(&d, None).unwrap();
    assert!(text.contains("| api_operation | 1 | 1 | 0 | 0 | 0 | 0 | 0 |"));
    assert!(text.contains("| behavior | 1 | 1 | 0 | 0 | 0 | 0 | 0 |"));
    assert!(text.contains("| project_requirement | 1 | 0 | 0 | 0 | 1 | 0 | 0 |"));
    assert!(
        text.contains("| TEST-001 | Test requirement | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |")
    );
    assert!(text.contains("Parameter and schema-field inventories are not additional features"));
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "gcs-mimic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn fixture() -> Temp {
    let tmp = Temp::new();
    for dir in ["compatibility", "docs/en", "planning/notes-ja"] {
        fs::create_dir_all(tmp.0.join(dir)).unwrap();
    }
    for file in [
        "compatibility/features.json",
        "compatibility/parameter-coverage.json",
        "compatibility/schema-field-coverage.json",
        "planning/notes-ja/feature-titles.json",
    ] {
        fs::copy(root().join(file), tmp.0.join(file)).unwrap();
    }
    let ledger = read_json(&root().join("compatibility/features.json")).unwrap();
    for row in ledger["features"].as_array().unwrap() {
        for check in row["verification"].as_array().unwrap() {
            let path = check["artifact"].as_str().unwrap();
            let destination = tmp.0.join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(root().join(path), destination).unwrap();
        }
    }
    tmp
}

#[test]
fn render_check_detects_stale_views_without_mutating() {
    let tmp = fixture();
    run(&tmp.0, "render").unwrap();
    run(&tmp.0, "check").unwrap();
    let english = fs::read_to_string(tmp.0.join("docs/en/feature-matrix.md")).unwrap();
    fs::write(tmp.0.join("planning/notes-ja/feature-matrix.md"), "stale").unwrap();
    assert!(run(&tmp.0, "check").unwrap_err().contains("stale"));
    assert_eq!(
        english,
        fs::read_to_string(tmp.0.join("docs/en/feature-matrix.md")).unwrap()
    );
    run(&tmp.0, "render").unwrap();
    run(&tmp.0, "check").unwrap();
}

#[test]
fn malformed_oversized_and_mismatched_inputs_fail() {
    let tmp = fixture();
    let input = tmp.0.join("bad.json");
    fs::write(&input, "{broken").unwrap();
    assert!(read_json(&input).is_err());
    let file = fs::File::create(&input).unwrap();
    file.set_len(8 * 1024 * 1024 + 1).unwrap();
    assert!(read_json(&input).unwrap_err().contains("8 MiB"));
    let title_file = tmp.0.join("planning/notes-ja/feature-titles.json");
    fs::write(&title_file, "{\"titles\":{}}").unwrap();
    assert!(
        run(&tmp.0, "validate")
            .unwrap_err()
            .contains("companion IDs")
    );
    let inv = tmp.0.join("compatibility/parameter-coverage.json");
    let mut data = read_json(&inv).unwrap();
    data["count"] = json!(0);
    fs::write(&inv, data.to_string()).unwrap();
    assert!(
        run(&tmp.0, "validate")
            .unwrap_err()
            .contains("inventory count")
    );
}

#[test]
fn evidence_artifacts_cannot_escape_repository() {
    let tmp = Temp::new();
    let d = completed();
    fs::write(tmp.0.join("README.md"), "").unwrap();
    assert!(validate(&tmp.0, &d).unwrap_err().contains("empty"));
    fs::write(tmp.0.join("README.md"), "Evidence fixture").unwrap();
    validate(&tmp.0, &d).unwrap();
    #[cfg(unix)]
    {
        fs::remove_file(tmp.0.join("README.md")).unwrap();
        std::os::unix::fs::symlink(root().join("README.md"), tmp.0.join("README.md")).unwrap();
        assert!(
            validate(&tmp.0, &d)
                .unwrap_err()
                .contains("inside the repository")
        );
    }
}

#[test]
fn cli_usage_and_errors_have_correct_exit_status() {
    let bin = env!("CARGO_BIN_EXE_xtask");
    assert!(
        Command::new(bin)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    for args in [
        vec![],
        vec!["compatibility", "unknown"],
        vec!["compatibility", "validate", "extra"],
    ] {
        let result = Command::new(bin)
            .args(args)
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!result.stderr.is_empty());
    }
    assert!(
        Command::new(bin)
            .args(["compatibility", "validate"])
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap()
            .status
            .success()
    );
}
