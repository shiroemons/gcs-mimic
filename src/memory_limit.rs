//! The RAM backend's default object budget, sized to the memory this process may use.
//!
//! The budget decides how a test that stores too much ends: with a 503 that names the setting, or
//! with the kernel killing the server and everything it held. Continuous integration usually gives
//! a container no limit, and a small fixed budget then fails ordinary suites for no benefit.

use std::path::Path;

const MIB: u64 = 1024 * 1024;
/// What every release before this rule used, so no environment loses capacity.
const MIN_BUDGET: u64 = 64 * MIB;
/// A thousand objects of 1 MiB, the scale this emulator is built for. A test storing more than it
/// meant to should still fail long before it takes over the machine.
const MAX_BUDGET: u64 = 1024 * MIB;
/// What the catalog's main and temporary databases may fill at their default page limits.
const CATALOG_PAGES: u64 = 32 * MIB;

/// Half of what the catalog leaves. Freed chunks of one size do not serve another, so deleting
/// every other object and refilling with a different size left about 1.8 times the budget resident.
pub(crate) fn default_budget() -> u64 {
    budget_within(available_bytes())
}

fn budget_within(available: Option<u64>) -> u64 {
    available.map_or(MAX_BUDGET, |bytes| {
        (bytes.saturating_sub(CATALOG_PAGES) / 2).clamp(MIN_BUDGET, MAX_BUDGET)
    })
}

/// The tightest of the machine's memory and the cgroup limits on this process. These files exist
/// only on Linux; a workstation on another system has memory to spare for the maximum.
fn available_bytes() -> Option<u64> {
    let read = |path: &Path| std::fs::read_to_string(path).ok();
    let membership = read(Path::new("/proc/self/cgroup")).unwrap_or_default();
    let machine = read(Path::new("/proc/meminfo")).and_then(|meminfo| machine_memory(&meminfo));
    machine
        .into_iter()
        .chain(cgroup_limit(&membership, read))
        .min()
}

fn machine_memory(meminfo: &str) -> Option<u64> {
    let total = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?;
    let kibibytes: u64 = total.split_whitespace().next()?.parse().ok()?;
    kibibytes.checked_mul(1024)
}

/// A version 2 limit binds every group below it, so each ancestor counts, and its "max" parses to
/// no limit. Version 1 reports no limit as a huge number, which the machine's memory undercuts.
fn cgroup_limit(membership: &str, read: impl Fn(&Path) -> Option<String>) -> Option<u64> {
    let root = Path::new("/sys/fs/cgroup");
    let group = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap_or("/");
    Path::new(group.trim_start_matches('/'))
        .ancestors()
        .map(|ancestor| root.join(ancestor).join("memory.max"))
        .chain([root.join("memory/memory.limit_in_bytes")])
        .filter_map(|file| read(&file)?.trim().parse().ok())
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, path::PathBuf};

    fn files<const N: usize>(entries: [(&str, &str); N]) -> impl Fn(&Path) -> Option<String> {
        let files: HashMap<PathBuf, String> = entries
            .into_iter()
            .map(|(path, contents)| (PathBuf::from(path), contents.to_owned()))
            .collect();
        move |path| files.get(path).cloned()
    }

    #[test]
    fn the_budget_is_half_of_what_the_catalog_leaves_within_its_bounds() {
        for (available, budget) in [
            (None, 1024),
            (Some(0), 64),
            (Some(128), 64),
            (Some(512), 240),
            (Some(1024), 496),
            (Some(2048), 1008),
            (Some(16 * 1024), 1024),
        ] {
            assert_eq!(
                budget_within(available.map(|mebibytes: u64| mebibytes * MIB)),
                budget * MIB,
                "{available:?}"
            );
        }
    }

    #[test]
    fn a_container_view_reads_its_own_limit() {
        let read = files([("/sys/fs/cgroup/memory.max", "536870912\n")]);
        assert_eq!(cgroup_limit("0::/\n", read), Some(512 * MIB));
    }

    #[test]
    fn an_unlimited_group_has_no_limit() {
        let read = files([("/sys/fs/cgroup/memory.max", "max\n")]);
        assert_eq!(cgroup_limit("0::/\n", read), None);
    }

    #[test]
    fn the_tightest_ancestor_binds_a_nested_group() {
        let read = files([
            ("/sys/fs/cgroup/docker/abc/memory.max", "max\n"),
            ("/sys/fs/cgroup/docker/memory.max", "268435456\n"),
        ]);
        assert_eq!(cgroup_limit("0::/docker/abc\n", read), Some(256 * MIB));
    }

    #[test]
    fn a_version_one_limit_counts() {
        let read = files([("/sys/fs/cgroup/memory/memory.limit_in_bytes", "134217728\n")]);
        assert_eq!(
            cgroup_limit("8:memory:/docker/abc\n", read),
            Some(128 * MIB)
        );
    }

    #[test]
    fn missing_files_leave_the_limit_unknown() {
        assert_eq!(cgroup_limit("", files([])), None);
        assert_eq!(machine_memory(""), None);
    }

    #[test]
    fn the_machine_total_is_read_in_kibibytes() {
        let meminfo = "MemFree:  1 kB\nMemTotal:       2097152 kB\n";
        assert_eq!(machine_memory(meminfo), Some(2048 * MIB));
    }
}
