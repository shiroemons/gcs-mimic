use super::*;
use crate::lint::{ADAPTER_DEPENDENCY, LAYER_DEPENDENCY, production_file, scan_source};
use std::collections::BTreeSet;

/// `(rule, line)` of each module-layer finding in `text` as the production file `file`.
fn layer_findings(file: &str, text: &str) -> Vec<(&'static str, usize)> {
    scan_source(file, text)
        .findings
        .into_iter()
        .filter_map(|finding| {
            [ADAPTER_DEPENDENCY, LAYER_DEPENDENCY]
                .into_iter()
                .find(|rule| *rule == finding.rule)
                .map(|rule| (rule, finding.line))
        })
        .collect()
}

#[test]
fn adapters_may_not_name_each_other() {
    let adapters = ["json", "xml", "grpc"];
    for from in adapters {
        for to in adapters {
            let text = format!(
                "use crate::{to}::Item;\nuse crate::{{bucket, {to} as adapter}};\nfn f() {{ crate::{to}::call(); }}\n"
            );
            let expected = if from == to {
                Vec::new()
            } else {
                vec![
                    (ADAPTER_DEPENDENCY, 1),
                    (ADAPTER_DEPENDENCY, 2),
                    (ADAPTER_DEPENDENCY, 3),
                ]
            };
            for file in [format!("src/{from}/mod.rs"), format!("src/{from}/v2/a.rs")] {
                assert_eq!(layer_findings(&file, &text), expected, "{file} naming {to}");
            }
        }
    }
    // Only paths from the crate root name a module: these name serde_json, a local or a
    // nested item.
    let unrelated = "\
use serde_json::{json, Value};
use other::{json::X, xml};
fn f() {
    let a = other::json::X;
    let b = reqwest.json::<T>();
    let c = json!({});
    let json = 1;
    let d = serde_json::json::x();
    let e = crate::bucket::json::X;
}
";
    assert_eq!(layer_findings("src/grpc/v2/a.rs", unrelated), []);
}

#[test]
fn same_and_lower_layers_are_allowed_and_higher_layers_are_reported() {
    let text = "\
use crate::{clock::Clock, bucket, storage::Catalog};
use crate::import::Journal;
use crate::config::Config;
use crate::server::State;
fn f() -> crate::json::JsonError { crate::page_token::decode(x) }
const V: &str = crate::VERSION;
crate::string_enum! { enum E { A } }
";
    assert_eq!(
        layer_findings("src/storage/catalog.rs", text),
        [
            (LAYER_DEPENDENCY, 4),
            (LAYER_DEPENDENCY, 5),
            (LAYER_DEPENDENCY, 5)
        ]
    );
    // An adapter naming a higher layer breaks the layering, not adapter isolation.
    assert_eq!(
        layer_findings("src/json/objects.rs", text),
        [(LAYER_DEPENDENCY, 4)]
    );
    assert_eq!(layer_findings("src/server/ingress.rs", text), []);
}

#[test]
fn super_and_self_resolve_from_the_file_path() {
    // `super` in src/server.rs is the crate root, and every other layer is below `server`.
    let text = "\
use super::{json, storage::Catalog, hex};
use super::server::State;
use self::accept::Slots;
";
    assert_eq!(layer_findings("src/server.rs", text), []);
    assert_eq!(
        layer_findings("src/clock.rs", text),
        [
            (LAYER_DEPENDENCY, 1),
            (LAYER_DEPENDENCY, 1),
            (LAYER_DEPENDENCY, 2)
        ]
    );
    let nested = "\
use super::super::super::{clock, server::State};
use super::super::super::json::JsonError;
use super::super::Catalog;
use super::super::super::super::beyond_the_root;
fn f() { super::super::super::server::call(); }
pub(super) fn g() {}
pub(in super::super) fn h() {}
";
    for file in [
        "src/storage/catalog/rows.rs",
        "src/storage/catalog/rows/mod.rs",
    ] {
        assert_eq!(
            layer_findings(file, nested),
            [
                (LAYER_DEPENDENCY, 1),
                (LAYER_DEPENDENCY, 2),
                (LAYER_DEPENDENCY, 5)
            ],
            "{file}"
        );
    }
    // An inline module adds a level that `super` climbs first.
    let inline = "\
mod inner {
    use super::server::State;
    use super::super::server::State;
}
use super::server::State;
";
    assert_eq!(
        layer_findings("src/clock.rs", inline),
        [(LAYER_DEPENDENCY, 3), (LAYER_DEPENDENCY, 5)]
    );
}

#[test]
fn dollar_crate_in_a_macro_is_reported_where_the_macro_is_written() {
    let text = "\
macro_rules! state {
    () => {
        $crate::server::State::new($crate::hex::encode(x))
    };
}
";
    assert_eq!(
        layer_findings("src/clock.rs", text),
        [(LAYER_DEPENDENCY, 3)]
    );
}

#[test]
fn only_the_top_level_members_of_a_crate_group_are_modules() {
    let text = "\
use crate::{
    hex::{server, json},
    server::{self, State},
    task,
    self,
};
use crate::{hex::{server::X}, json};
";
    assert_eq!(
        layer_findings("src/clock.rs", text),
        [(LAYER_DEPENDENCY, 3), (LAYER_DEPENDENCY, 7)]
    );
}

#[test]
fn re_exports_are_reported_where_they_are_written() {
    let text = "pub(crate) use crate::server::State;\npub use crate::json::JsonError as Error;\n";
    assert_eq!(
        layer_findings("src/bucket.rs", text),
        [(LAYER_DEPENDENCY, 1), (LAYER_DEPENDENCY, 2)]
    );
}

#[test]
fn tests_comments_strings_binary_files_and_the_library_root_are_skipped() {
    let text = "\
// use crate::server::State;
/// [`crate::server::State`]
const PATH: &str = \"crate::server::State\";
pub(crate) fn f(self) {}
#[cfg(test)]
mod tests {
    use crate::server::State;
}
#[cfg(all(test, unix))]
fn helper() { super::server::call(); }
";
    assert_eq!(layer_findings("src/clock.rs", text), []);
    let reference = "use crate::server::State;\nfn f() { crate::json::call(); }\n";
    for file in ["src/main.rs", "src/lib.rs"] {
        assert_eq!(layer_findings(file, reference), [], "{file}");
    }
    // The binary is another crate and declares no modules of its own; the command line is the
    // library's highest layer, so it may name every module and no module may name it.
    assert_eq!(
        declared_modules(include_str!("../../../../src/main.rs")),
        Vec::<&str>::new()
    );
    for file in ["src/app.rs", "src/app/startup/authentication.rs"] {
        assert_eq!(layer_findings(file, reference), [], "{file}");
    }
    assert_eq!(
        layer_findings("src/server.rs", "fn f() { crate::app::main(); }\n"),
        [(LAYER_DEPENDENCY, 1)]
    );
    for file in [
        "src/clock/tests.rs",
        "src/storage/catalog_tests.rs",
        "src/test_support.rs",
    ] {
        assert!(!production_file(file), "{file}");
    }
}

#[test]
fn every_library_module_has_exactly_one_layer() {
    assert_eq!(
        table_problems(include_str!("../../../../src/lib.rs")),
        Vec::<String>::new()
    );
    let mut seen = BTreeSet::new();
    for module in LAYERS.iter().flat_map(|layer| layer.modules) {
        assert!(seen.insert(module), "{module} is in two layers");
    }
    let declared: String = LAYERS
        .iter()
        .flat_map(|layer| layer.modules)
        .map(|module| format!("pub(crate) mod {module};\n"))
        .collect();
    assert_eq!(table_problems(&declared), Vec::<String>::new());
    assert_eq!(
        table_problems(&format!("{declared}#[cfg(test)]\nmod test_support;\n")),
        Vec::<String>::new()
    );
    let unlayered = table_problems(&format!(
        "{declared}// mod comment;\nmod unassigned_for_test;\n"
    ));
    assert_eq!(unlayered.len(), 1, "{unlayered:?}");
    assert!(
        unlayered[0].contains("module `unassigned_for_test`"),
        "{unlayered:?}"
    );
    let undeclared = table_problems(&declared.replace("pub(crate) mod hex;\n", ""));
    assert_eq!(
        undeclared,
        ["layer `foundation` names module `hex`, which src/lib.rs does not declare"]
    );
}

/// The design guide shows the table that the gate enforces.
#[test]
fn the_design_guide_shows_the_same_layer_table() {
    // A Windows checkout may end lines with CRLF.
    let design = include_str!("../../../../docs/en/design.md").replace("\r\n", "\n");
    let (_, table) = design
        .split_once("| Layer | Modules |\n")
        .expect("docs/en/design.md has the module layer table");
    let rows: Vec<&str> = table
        .lines()
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .collect();
    let expected: Vec<String> = LAYERS
        .iter()
        .map(|layer| {
            let name = match layer.peers {
                Peers::Shared => layer.name.to_owned(),
                Peers::Isolated => format!("{} (isolated)", layer.name),
            };
            let modules: Vec<String> = layer
                .modules
                .iter()
                .map(|module| format!("`{module}`"))
                .collect();
            format!("| {name} | {} |", modules.join(", "))
        })
        .collect();
    assert_eq!(rows, expected);
}

#[test]
fn both_rules_take_a_reasoned_suppression() {
    let text = "\
// xtask-lint: allow(text::layer_dependency) -- moves below the adapters with the shared server pieces
use crate::server::State;
use crate::xml::Item; // xtask-lint: allow(text::adapter_dependency) -- shared until the helper moves
use crate::grpc::Item;
";
    let scan = scan_source("src/json/a.rs", text);
    assert_eq!(scan.problems, []);
    assert_eq!(
        scan.findings
            .iter()
            .map(|f| (f.rule.as_str(), f.line))
            .collect::<Vec<_>>(),
        [(ADAPTER_DEPENDENCY, 4)]
    );
    assert_eq!(
        scan.suppressed
            .iter()
            .map(|f| (f.rule.as_str(), f.line))
            .collect::<Vec<_>>(),
        [(LAYER_DEPENDENCY, 2), (ADAPTER_DEPENDENCY, 3)]
    );
    let stale = scan_source(
        "src/json/a.rs",
        "// xtask-lint: allow(text::adapter_dependency) -- shared until the helper moves\nuse crate::bucket::Bucket;\n",
    );
    assert_eq!(stale.problems.len(), 1, "{:?}", stale.problems);
}
