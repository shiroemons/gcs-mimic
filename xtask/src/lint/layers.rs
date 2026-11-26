//! Module layers: which of the library's modules a module may name.
//!
//! A module may name modules in its own layer and in lower layers. The modules of an isolated
//! layer, the three protocol adapters, may not name each other either. A reference to a higher
//! layer is `text::layer_dependency` and one between isolated peers is
//! `text::adapter_dependency`; two rules rather than one keep a file from trading one forbidden
//! edge for another under an unchanged per-file count. [`LAYERS`] lists exactly the modules that
//! `src/lib.rs` declares, and `docs/en/design.md` shows the same table.

use super::{close_expression, ident_at, line_index, mask, skip_space, test_lines, words};
use std::{fs, ops::Range, path::Path};

/// The library root. Its declarations must match [`LAYERS`]; its own code only declares
/// modules and re-exports names for the binary and the integration tests, so it is not scanned.
const LIB_FILE: &str = "src/lib.rs";

/// Whether the modules of one layer may name each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Peers {
    Shared,
    /// Adapters that depend on each other cannot be reasoned about or replaced separately.
    Isolated,
}

#[derive(Debug)]
struct Layer {
    name: &'static str,
    peers: Peers,
    modules: &'static [&'static str],
}

/// The layers from the lowest to the highest. The binary's modules (`src/main.rs` and the
/// modules it declares) are another crate and stay outside the table.
const LAYERS: [Layer; 7] = [
    Layer {
        name: "foundation",
        peers: Peers::Shared,
        modules: &[
            "base64",
            "hex",
            "percent",
            "decimal",
            "clock",
            "hashing",
            "serde_util",
            "error",
            "task",
            "diagnostics",
            "fsutil",
            "bounded_body",
            "bounded_file",
            "time_limit",
            "memory_limit",
            "fd_limit",
            "net",
            "match_glob",
            "source_tree",
            "mime",
            "http_conditions",
            "http_header",
        ],
    },
    Layer {
        name: "model",
        peers: Peers::Shared,
        modules: &[
            "project",
            "acl",
            "bucket",
            "object",
            "metadata_error",
            "etag",
            "csek",
            "auth",
            "url",
            "notification",
            "time_format",
            "input",
        ],
    },
    Layer {
        name: "settings",
        peers: Peers::Shared,
        modules: &["config", "config_input", "tls"],
    },
    Layer {
        name: "storage",
        peers: Peers::Shared,
        modules: &["storage", "import", "seed", "bucket_init"],
    },
    Layer {
        name: "service",
        peers: Peers::Shared,
        modules: &["page_token", "multipart", "api_error", "service"],
    },
    Layer {
        name: "adapters",
        peers: Peers::Isolated,
        modules: &["json", "xml", "grpc"],
    },
    Layer {
        name: "transport",
        peers: Peers::Shared,
        modules: &["server"],
    },
];

/// How a reference breaks the layering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Breach {
    /// A module names a module in a higher layer.
    HigherLayer,
    /// A module names another module of an isolated layer.
    IsolatedPeer,
}

/// The layer's position from the bottom, and the layer, of a top-level module.
fn layer_of(module: &str) -> Option<(usize, &'static Layer)> {
    LAYERS
        .iter()
        .enumerate()
        .find(|(_, layer)| layer.modules.contains(&module))
}

/// Offsets in `masked`, the masked text of the production file `file`, of each reference that
/// breaks the layering. Files of modules without a layer (the binary's) and `src/lib.rs` have
/// none.
pub(super) fn breaches(file: &str, masked: &str) -> Vec<(Breach, usize)> {
    let path = module_path(file);
    let Some(&own) = path.first() else {
        return Vec::new();
    };
    let Some((rank, layer)) = layer_of(own) else {
        return Vec::new();
    };
    let inline = inline_modules(masked);
    references(&path, &inline, masked)
        .into_iter()
        .filter_map(|(target, offset)| {
            let (target_rank, _) = layer_of(target)?;
            let breach = if target_rank > rank {
                Breach::HigherLayer
            } else if target_rank == rank && target != own && layer.peers == Peers::Isolated {
                Breach::IsolatedPeer
            } else {
                return None;
            };
            Some((breach, offset))
        })
        .collect()
}

/// The module path of a source file under `src/`: `src/a/b.rs` and `src/a/b/mod.rs` are
/// `a::b`, and `src/lib.rs` is the crate root. `#[path]` is rejected elsewhere, so the file
/// tree mirrors the module tree.
fn module_path(file: &str) -> Vec<&str> {
    let Some(relative) = file
        .strip_prefix("src/")
        .and_then(|rest| rest.strip_suffix(".rs"))
    else {
        return Vec::new();
    };
    if relative == "lib" {
        return Vec::new();
    }
    let mut path: Vec<&str> = relative.split('/').collect();
    if path.last() == Some(&"mod") {
        path.pop();
    }
    path
}

/// The body ranges and names of inline `mod name { .. }` items, which `super::` resolves
/// against.
fn inline_modules(masked: &str) -> Vec<(Range<usize>, &str)> {
    let bytes = masked.as_bytes();
    words(masked, "mod")
        .filter_map(|at| {
            let start = skip_space(bytes, at + "mod".len());
            let name = ident_at(masked, start);
            let open = skip_space(bytes, start + name.len());
            if name.is_empty() || bytes.get(open) != Some(&b'{') {
                return None;
            }
            Some((open..close_expression(bytes, open)?, name))
        })
        .collect()
}

/// Top-level modules named from the crate root, `$crate`, `super` or `self`, with the offset
/// of the name that selects each one. A path that stays below the crate root names the
/// module it stays in; one that reaches the root names the segment after it, or each member
/// of a `{..}` group there.
fn references<'a>(
    file_path: &[&'a str],
    inline: &[(Range<usize>, &'a str)],
    masked: &'a str,
) -> Vec<(&'a str, usize)> {
    let mut found = Vec::new();
    for root in ["crate", "super", "self"] {
        for start in words(masked, root) {
            // `a::super` continues a chain whose root was already resolved.
            if masked[..start].trim_end().ends_with("::") {
                continue;
            }
            let mut base: Vec<&str> = if root == "crate" {
                Vec::new()
            } else {
                scope(file_path, inline, start)
            };
            if root == "super" && base.pop().is_none() {
                continue;
            }
            let Some((base, next)) = resolve(masked, base, start + root.len()) else {
                continue;
            };
            match base.first() {
                Some(&module) => found.push((module, next)),
                None if masked.as_bytes().get(next) == Some(&b'{') => {
                    found.extend(group_members(masked, next));
                }
                None => match ident_at(masked, next) {
                    "" | "self" => {}
                    module => found.push((module, next)),
                },
            }
        }
    }
    found
}

/// The module path at `offset`: the file's path plus the inline modules around it.
fn scope<'a>(
    file_path: &[&'a str],
    inline: &[(Range<usize>, &'a str)],
    offset: usize,
) -> Vec<&'a str> {
    let mut path = file_path.to_vec();
    path.extend(
        inline
            .iter()
            .filter(|(body, _)| body.contains(&offset))
            .map(|(_, name)| *name),
    );
    path
}

/// Follows `::super` segments after a path root that ends at `cursor`, returning the module
/// path reached and the offset of the next segment. `None` when no `::` follows the root (as
/// in `pub(crate)` or `{self, ..}`) or the chain climbs past the crate root.
fn resolve<'a>(
    masked: &str,
    mut base: Vec<&'a str>,
    mut cursor: usize,
) -> Option<(Vec<&'a str>, usize)> {
    let bytes = masked.as_bytes();
    loop {
        let separator = skip_space(bytes, cursor);
        if !masked[separator..].starts_with("::") {
            return None;
        }
        let next = skip_space(bytes, separator + 2);
        if ident_at(masked, next) != "super" {
            return Some((base, next));
        }
        base.pop()?;
        cursor = next + "super".len();
    }
}

/// The leading name of each member of the `{..}` group opening at `open`, with its offset;
/// `self` names the group's own path and is skipped.
fn group_members(masked: &str, open: usize) -> Vec<(&str, usize)> {
    let bytes = masked.as_bytes();
    let Some(close) = close_expression(bytes, open) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut depth = 0usize;
    let mut member_start = open + 1;
    for (i, &byte) in bytes.iter().enumerate().take(close - 1).skip(open + 1) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                members.push(member_start);
                member_start = i + 1;
            }
            _ => {}
        }
    }
    members.push(member_start);
    members
        .into_iter()
        .filter_map(|start| {
            let at = skip_space(bytes, start);
            match ident_at(masked, at) {
                "" | "self" => None,
                name => Some((name, at)),
            }
        })
        .collect()
}

/// Reads `src/lib.rs` under `root` and returns [`table_problems`] for it.
pub(super) fn check(root: &Path) -> Result<Vec<String>, String> {
    let lib = fs::read_to_string(root.join(LIB_FILE)).map_err(|e| format!("{LIB_FILE}: {e}"))?;
    Ok(table_problems(&lib))
}

/// Disagreements between [`LAYERS`] and the modules `lib` declares outside `#[cfg(test)]`. A
/// module without a layer would escape both rules, and a stale table entry would hide a
/// typo.
fn table_problems(lib: &str) -> Vec<String> {
    let declared = declared_modules(lib);
    let mut problems = Vec::new();
    for module in &declared {
        let layers: Vec<&str> = LAYERS
            .iter()
            .filter(|layer| layer.modules.contains(module))
            .map(|layer| layer.name)
            .collect();
        match layers[..] {
            [_] => {}
            [] => problems.push(format!(
                "{LIB_FILE} declares module `{module}`, which no layer in xtask/src/lint/layers.rs names; give it a layer there and in docs/en/design.md"
            )),
            _ => problems.push(format!(
                "module `{module}` is in more than one layer: {}",
                layers.join(", ")
            )),
        }
    }
    for layer in &LAYERS {
        for module in layer.modules {
            if !declared.contains(module) {
                problems.push(format!(
                    "layer `{}` names module `{module}`, which {LIB_FILE} does not declare",
                    layer.name
                ));
            }
        }
    }
    problems
}

/// The names of the `mod name;` and `mod name { .. }` items in `lib` that are not test-only.
fn declared_modules(lib: &str) -> Vec<&str> {
    let masked = mask(lib);
    let bytes = masked.as_bytes();
    let tests = test_lines(&masked);
    words(&masked, "mod")
        .filter(|&at| !tests.get(line_index(&masked, at)).copied().unwrap_or(false))
        .filter_map(|at| {
            let start = skip_space(bytes, at + "mod".len());
            let end = start + ident_at(&masked, start).len();
            if end == start || !matches!(bytes.get(skip_space(bytes, end)), Some(b';' | b'{')) {
                return None;
            }
            // Masking keeps identifier bytes and offsets, so the name can come from `lib`.
            lib.get(start..end)
        })
        .collect()
}

#[cfg(test)]
mod tests;
