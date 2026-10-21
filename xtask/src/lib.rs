use serde_json::Value;
pub mod container;
pub mod dependencies;
pub mod release;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path},
};

type Result<T> = std::result::Result<T, String>;
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;
const KINDS: [&str; 3] = ["api_operation", "behavior", "project_requirement"];
const STATUSES: [&str; 6] = [
    "NOT_STARTED",
    "IN_PROGRESS",
    "PARTIAL",
    "IMPLEMENTED",
    "BLOCKED",
    "NOT_APPLICABLE",
];
const FIDELITIES: [&str; 5] = [
    "EXACT",
    "LOCAL_SIMULATION",
    "METADATA_ONLY",
    "EXTERNAL",
    "PROJECT",
];

pub fn read_json(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    ensure(bytes.len() as u64 <= MAX_JSON_BYTES, "JSON exceeds 8 MiB")?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key}: expected nonempty string"))
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value[key]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{key}: expected array"))
}

fn strings<'a>(value: &'a Value, key: &str) -> Result<BTreeSet<&'a str>> {
    let items = array(value, key)?;
    let mut result = BTreeSet::new();
    for item in items {
        let text = item
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("{key}: expected nonempty string entries"))?;
        ensure(result.insert(text), &format!("{key}: duplicate {text}"))?;
    }
    Ok(result)
}

fn date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = text[..4].parse().unwrap();
    let month: u32 = text[5..7].parse().unwrap();
    let day: u32 = text[8..].parse().unwrap();
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => 0,
    };
    year > 0 && day > 0 && day <= days
}

fn artifact(root: &Path, name: &str) -> Result<()> {
    let path = Path::new(name);
    ensure(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "artifact must be a repository-relative path without traversal",
    )?;
    let actual = root
        .join(path)
        .canonicalize()
        .map_err(|e| format!("{name}: {e}"))?;
    ensure(
        actual.starts_with(root.canonicalize().map_err(|e| e.to_string())?) && actual.is_file(),
        "artifact must be a file inside the repository",
    )?;
    ensure(
        fs::metadata(actual).map_err(|e| e.to_string())?.len() > 0,
        "artifact is empty",
    )
}

fn evidence(root: &Path, row: &Value) -> Result<()> {
    let cases = array(row, "acceptance_cases")?;
    let tests = strings(row, "test_ids")?;
    let checks = array(row, "verification")?;
    let complete = row["status"] == "IMPLEMENTED";
    if complete {
        ensure(
            !cases.is_empty() && !tests.is_empty() && !checks.is_empty(),
            "IMPLEMENTED requires acceptance cases, test IDs and verification",
        )?;
        ensure(
            array(row, "known_deviations")?.is_empty(),
            "IMPLEMENTED has unresolved deviations",
        )?;
        ensure(
            row["acceptance_status"] == "COMPLETE",
            "IMPLEMENTED requires COMPLETE acceptance",
        )?;
        ensure(
            date(string(row, "last_verified")?),
            "invalid last_verified date",
        )?;
        ensure(
            row["actual_fidelity"] == row["target_fidelity"],
            "IMPLEMENTED must meet target fidelity",
        )?;
    }
    let mut proven = BTreeSet::new();
    for check in checks {
        string(check, "command")?;
        ensure(
            ["PASS", "FAIL"].contains(&string(check, "result")?),
            "invalid verification result",
        )?;
        ensure(
            date(string(check, "verified_at")?),
            "invalid verification date",
        )?;
        artifact(root, string(check, "artifact")?)?;
        let ids = strings(check, "test_ids")?;
        ensure(
            !ids.is_empty() && ids.is_subset(&tests),
            "verification references unknown/empty test IDs",
        )?;
        if check["result"] == "PASS" {
            proven.extend(ids);
        }
        if complete {
            ensure(
                check["result"] == "PASS",
                "IMPLEMENTED has failed verification",
            )?;
        }
    }
    let mut case_ids = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for case in cases {
        ensure(
            case_ids.insert(string(case, "id")?),
            "duplicate acceptance case ID",
        )?;
        string(case, "description")?;
        ensure(
            ["PENDING", "PASS", "FAIL"].contains(&string(case, "result")?),
            "invalid acceptance result",
        )?;
        let ids = strings(case, "test_ids")?;
        ensure(
            ids.is_subset(&tests),
            "acceptance references unknown test IDs",
        )?;
        if case["result"] == "PASS" {
            ensure(
                !ids.is_empty() && ids.is_subset(&proven),
                "passing acceptance lacks passing evidence",
            )?;
        }
        if complete {
            ensure(
                case["result"] == "PASS",
                "IMPLEMENTED has unpassed acceptance",
            )?;
        }
        covered.extend(ids);
    }
    if complete {
        ensure(
            covered == tests && proven == tests,
            "IMPLEMENTED has unlinked test IDs",
        )?;
    }
    Ok(())
}

pub fn validate(root: &Path, ledger: &Value) -> Result<()> {
    ensure(ledger["schema_version"] == 1, "unsupported schema_version")?;
    ensure(date(string(ledger, "as_of")?), "invalid as_of date")?;
    let rows = array(ledger, "features")?;
    ensure(!rows.is_empty(), "empty feature ledger")?;
    let mut by_id = BTreeMap::new();
    let mut kinds: BTreeMap<&str, usize> = KINDS.into_iter().map(|k| (k, 0)).collect();
    let mut categories = BTreeMap::new();
    for row in rows {
        let id = string(row, "id")?;
        let check = || -> Result<()> {
            ensure(
                id.as_bytes()[0].is_ascii_uppercase()
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-'),
                "invalid ID",
            )?;
            ensure(!by_id.contains_key(id), "duplicate feature ID")?;
            let kind = string(row, "kind")?;
            ensure(KINDS.contains(&kind), "invalid kind")?;
            ensure(STATUSES.contains(&string(row, "status")?), "invalid status")?;
            ensure(
                FIDELITIES.contains(&string(row, "target_fidelity")?),
                "invalid target fidelity",
            )?;
            let actual = string(row, "actual_fidelity")?;
            ensure(
                actual == "NONE" || FIDELITIES.contains(&actual),
                "invalid actual fidelity",
            )?;
            ensure(
                kind == "project_requirement" || row["target_fidelity"] != "PROJECT",
                "kind/target fidelity mismatch",
            )?;
            ensure(
                (actual != "PROJECT" || row["target_fidelity"] == "PROJECT")
                    && (row["target_fidelity"] != "PROJECT"
                        || ["NONE", "PROJECT"].contains(&actual)),
                "kind/actual fidelity mismatch",
            )?;
            ensure(
                row["target_fidelity"] != "LOCAL_SIMULATION" || actual != "EXACT",
                "simulation cannot claim EXACT",
            )?;
            if row["status"] == "NOT_STARTED" {
                ensure(actual == "NONE", "NOT_STARTED has achieved fidelity")?;
            }
            ensure(
                ["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7"].contains(&string(row, "phase")?),
                "invalid phase",
            )?;
            string(row, "title")?;
            string(row, "category")?;
            ensure(
                date(string(row, "source_checked_at")?),
                "invalid source date",
            )?;
            let urls = strings(row, "source_urls")?;
            ensure(
                urls.iter()
                    .all(|u| u.starts_with("https://") && u.len() > 8),
                "sources require HTTPS URLs",
            )?;
            ensure(
                !urls.is_empty()
                    || (kind == "project_requirement"
                        && row["source_evidence"] == "PROPOSED_PROJECT_REQUIREMENT"),
                "missing source provenance",
            )?;
            string(row, "source_evidence")?;
            strings(row, "dependencies")?;
            array(row, "sdk_evidence")?;
            array(row, "known_deviations")?;
            if ["IN_PROGRESS", "PARTIAL", "IMPLEMENTED"].contains(&string(row, "status")?) {
                ensure(
                    !array(row, "acceptance_cases")?.is_empty(),
                    "active work needs acceptance decomposition",
                )?;
            }
            if row["status"] == "PARTIAL" {
                ensure(
                    !array(row, "known_deviations")?.is_empty(),
                    "PARTIAL needs explicit missing cases",
                )?;
            }
            evidence(root, row)
        };
        check().map_err(|e| format!("{id}: {e}"))?;
        *kinds.get_mut(string(row, "kind")?).unwrap() += 1;
        if row["kind"] == "api_operation" {
            *categories.entry(string(row, "category")?).or_insert(0usize) += 1;
        }
        by_id.insert(id, row);
    }
    for (kind, count) in kinds {
        ensure(
            ledger["counts_by_kind"][kind].as_u64() == Some(count as u64),
            &format!("counts_by_kind mismatch: {kind}"),
        )?;
    }
    ensure(
        ledger["api_counts_by_category"]
            .as_object()
            .is_some_and(|m| m.len() == categories.len()),
        "API category count mismatch",
    )?;
    for (category, count) in categories {
        ensure(
            ledger["api_counts_by_category"][category].as_u64() == Some(count as u64),
            "API category total mismatch",
        )?;
    }
    // Kahn's algorithm avoids a recursive stack proportional to user-edited dependency depth.
    let mut pending = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (&id, row) in &by_id {
        let deps = strings(row, "dependencies")?;
        for &dependency in &deps {
            let other = by_id
                .get(dependency)
                .ok_or_else(|| format!("{id}: unknown dependency {dependency}"))?;
            if row["status"] == "IMPLEMENTED" {
                ensure(
                    other["status"] == "IMPLEMENTED",
                    &format!("{id}: incomplete dependency {dependency}"),
                )?;
            }
            dependents.entry(dependency).or_default().push(id);
        }
        pending.insert(id, deps.len());
    }
    let mut ready: Vec<_> = pending
        .iter()
        .filter_map(|(&id, &n)| (n == 0).then_some(id))
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        if let Some(children) = dependents.get(id) {
            for child in children {
                let n = pending.get_mut(child).unwrap();
                *n -= 1;
                if *n == 0 {
                    ready.push(child);
                }
            }
        }
    }
    ensure(visited == rows.len(), "dependency cycle")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "&#92;")
        .replace('|', "&#124;")
        .replace('`', "&#96;")
        .replace('*', "&#42;")
        .replace('_', "&#95;")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
        .replace(['\r', '\n'], " ")
}

pub fn render(ledger: &Value, titles: Option<&Value>) -> Result<String> {
    let rows = array(ledger, "features")?;
    let mut result = String::from(
        "# Feature implementation matrix\n\nGenerated from `compatibility/features.json`; do not edit status here.\n\n",
    );
    result.push_str(&format!(
        "Research cutoff: {}. Implementation status and achieved fidelity are independent.\n\n",
        string(ledger, "as_of")?
    ));
    result.push_str("API operations, behaviors and project requirements have separate denominators. Parameter and schema-field inventories are not additional features. SDK evidence is recorded separately on each row.\n\n");
    result.push_str("| Kind | Rows | NOT_STARTED | IN_PROGRESS | PARTIAL | IMPLEMENTED | BLOCKED | NOT_APPLICABLE |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for kind in KINDS {
        let group: Vec<_> = rows.iter().filter(|r| r["kind"] == kind).collect();
        result.push_str(&format!("| {kind} | {} |", group.len()));
        for status in STATUSES {
            result.push_str(&format!(
                " {} |",
                group.iter().filter(|r| r["status"] == status).count()
            ));
        }
        result.push('\n');
    }
    result.push_str("\n| Kind | NONE | EXACT | LOCAL_SIMULATION | METADATA_ONLY | EXTERNAL | PROJECT |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for kind in KINDS {
        result.push_str(&format!("| {kind} |"));
        for fidelity in [
            "NONE",
            "EXACT",
            "LOCAL_SIMULATION",
            "METADATA_ONLY",
            "EXTERNAL",
            "PROJECT",
        ] {
            result.push_str(&format!(
                " {} |",
                rows.iter()
                    .filter(|r| r["kind"] == kind && r["actual_fidelity"] == fidelity)
                    .count()
            ));
        }
        result.push('\n');
    }
    for kind in KINDS {
        result.push_str(&format!("\n## {kind}\n"));
        let mut categories: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
        for row in rows.iter().filter(|r| r["kind"] == kind) {
            categories
                .entry(string(row, "category")?)
                .or_default()
                .push(row);
        }
        for (category, mut group) in categories {
            group.sort_by_key(|r| r["id"].as_str().unwrap());
            result.push_str(&format!("\n### {}\n\n| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |\n| --- | --- | --- | --- | --- | --- | ---: |\n", escape(category)));
            for row in group {
                let id = string(row, "id")?;
                let title = match titles {
                    Some(t) => t["titles"][id]
                        .as_str()
                        .ok_or_else(|| format!("missing companion title: {id}"))?,
                    None => string(row, "title")?,
                };
                result.push_str(&format!(
                    "| {id} | {} | {} | {} | {} | {} | {} |\n",
                    escape(title),
                    string(row, "status")?,
                    string(row, "phase")?,
                    string(row, "target_fidelity")?,
                    string(row, "actual_fidelity")?,
                    array(row, "sdk_evidence")?.len()
                ));
            }
        }
    }
    Ok(result)
}

fn validate_inventory(root: &Path, file: &str, key: &str) -> Result<usize> {
    let inventory = read_json(&root.join(file))?;
    let rows = array(&inventory, key)?;
    ensure(
        inventory["count"].as_u64() == Some(rows.len() as u64),
        "inventory count mismatch",
    )?;
    let mut ids = BTreeSet::new();
    for row in rows {
        ensure(ids.insert(string(row, "id")?), "duplicate inventory ID")?;
        ensure(
            STATUSES.contains(&string(row, "status")?),
            "invalid inventory status",
        )?;
        // These inventories have no fidelity or test-ID fields; their evidence is validated separately.
        if row["status"] == "IMPLEMENTED" {
            ensure(
                !array(row, "acceptance_cases")?.is_empty()
                    && !array(row, "verification")?.is_empty(),
                "implemented inventory item lacks evidence",
            )?;
        }
    }
    Ok(rows.len())
}

pub fn run(root: &Path, command: &str) -> Result<()> {
    ensure(
        ["validate", "render", "check"].contains(&command),
        "unknown compatibility command",
    )?;
    let ledger = read_json(&root.join("compatibility/features.json"))?;
    validate(root, &ledger)?;
    let parameters =
        validate_inventory(root, "compatibility/parameter-coverage.json", "parameters")?;
    let fields = validate_inventory(root, "compatibility/schema-field-coverage.json", "fields")?;
    let titles = read_json(&root.join("planning/notes-ja/feature-titles.json"))?;
    let title_map = titles["titles"].as_object().ok_or("missing title map")?;
    let rows = array(&ledger, "features")?;
    ensure(
        title_map.len() == rows.len()
            && rows.iter().all(|r| {
                title_map
                    .get(r["id"].as_str().unwrap())
                    .is_some_and(|t| t.as_str().is_some_and(|s| !s.trim().is_empty()))
            }),
        "companion IDs must match the ledger exactly",
    )?;
    for (path, content) in [
        ("docs/en/feature-matrix.md", render(&ledger, None)?),
        (
            "planning/notes-ja/feature-matrix.md",
            render(&ledger, Some(&titles))?,
        ),
    ] {
        if command == "render" {
            fs::write(root.join(path), content).map_err(|e| format!("{path}: {e}"))?;
        } else if command == "check" {
            let current =
                fs::read_to_string(root.join(path)).map_err(|e| format!("{path}: {e}"))?;
            ensure(
                current == content,
                &format!("{path} is stale; run cargo xtask compatibility render"),
            )?;
        }
    }
    println!(
        "Validated {} feature rows; {parameters} parameter locations and {fields} schema fields are separate inventories.",
        rows.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_enumerations_match_validator() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let schema = read_json(&root.join("compatibility/features.schema.json")).unwrap();
        let properties = &schema["properties"]["features"]["items"]["properties"];
        assert_eq!(properties["kind"]["enum"], serde_json::json!(KINDS));
        assert_eq!(properties["status"]["enum"], serde_json::json!(STATUSES));
        assert_eq!(
            properties["target_fidelity"]["enum"],
            serde_json::json!(FIDELITIES)
        );
        assert_eq!(
            properties["actual_fidelity"]["enum"],
            serde_json::json!([
                "NONE",
                "EXACT",
                "LOCAL_SIMULATION",
                "METADATA_ONLY",
                "EXTERNAL",
                "PROJECT"
            ])
        );
    }
}
