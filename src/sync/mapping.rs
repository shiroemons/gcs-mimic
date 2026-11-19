#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use super::naming::RootNaming;
use crate::storage::admin::{BucketName, ObjectName};

pub const MARKER: &str = ".gcs-mimic-sync.json";
pub(crate) const MARKER_STAGING: &str = ".gcs-mimic-sync.json.staging";
pub(crate) const WORKSPACE: &str = ".gcs-mimic-sync-work";
pub(crate) const RESERVED_ROOT_ENTRIES: usize = 3;
pub(crate) fn is_reserved_root_entry(name: &std::ffi::OsStr) -> bool {
    matches!(name.to_str(), Some(MARKER | MARKER_STAGING | WORKSPACE))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    InvalidName,
    HostRestriction,
    Reserved,
    Ignored,
    Collision,
    UnsafeEntry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathMapping {
    File(PathBuf),
    Unrepresentable(Reason),
}

/// Maps an object name to its path under the root's naming rules. Callers must still check
/// aliases, collisions and no-follow identities when they open the path.
pub(crate) fn name_to_path(
    bucket: &BucketName,
    object: &ObjectName,
    naming: RootNaming,
) -> PathMapping {
    match candidate(bucket, object, naming) {
        Ok(path) => PathMapping::File(path),
        Err(reason) => PathMapping::Unrepresentable(reason),
    }
}

fn candidate(
    bucket: &BucketName,
    object: &ObjectName,
    naming: RootNaming,
) -> Result<PathBuf, Reason> {
    let (bucket, object) = (bucket.as_str(), object.as_str());
    let mut path = PathBuf::new();
    for component in std::iter::once(bucket).chain(object.split('/')) {
        if component.is_empty() || matches!(component, "." | "..") || component.contains('\0') {
            return Err(Reason::InvalidName);
        }
        if is_reserved_root_entry(component.as_ref()) {
            return Err(Reason::Reserved);
        }
        // The catalog's sync-path validator rejects a backslash on every platform, so a
        // candidate it would refuse must be skipped here instead of stopping the root. What the
        // root's filesystem cannot hold is skipped the same way, before any I/O.
        if component.contains('\\') || !naming.admits(component.as_ref()) {
            return Err(Reason::HostRestriction);
        }
        #[cfg(windows)]
        if windows_component_is_invalid(component) {
            return Err(Reason::HostRestriction);
        }
        path.push(component);
    }
    if object
        .rsplit('/')
        .next()
        .is_some_and(crate::source_tree::is_ignored_regular_file)
    {
        return Err(Reason::Ignored);
    }
    Ok(path)
}

/// Decodes only the exact relative path spelling that `name_to_path` emits under the same rules.
pub(crate) fn path_to_name(
    path: &Path,
    naming: RootNaming,
) -> Result<(BucketName, ObjectName), Reason> {
    let raw = path.to_str().ok_or(Reason::InvalidName)?;
    #[cfg(windows)]
    let text = raw.replace('\\', "/");
    #[cfg(not(windows))]
    let text = raw;
    let (bucket, object) = text.split_once('/').ok_or(Reason::InvalidName)?;
    let bucket = BucketName::new(bucket).map_err(|_| Reason::InvalidName)?;
    let object = ObjectName::new(object).map_err(|_| Reason::InvalidName)?;
    let candidate = candidate(&bucket, &object, naming)?;
    if candidate.as_os_str() != path.as_os_str() {
        return Err(Reason::InvalidName);
    }
    Ok((bucket, object))
}

#[cfg(windows)]
fn windows_component_is_invalid(component: &str) -> bool {
    if component
        .chars()
        .any(|c| c < ' ' || matches!(c, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
        || component.ends_with([' ', '.'])
    {
        return true;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    use crate::sync::naming::{Case, Colon, NameMax, Observation, Spelling};

    fn naming(colon: Colon, name_max: Option<usize>) -> RootNaming {
        RootNaming::try_from(Observation {
            colon,
            case: Case::Sensitive,
            spelling: Spelling::Preserved,
            name_max: NameMax::from(name_max),
        })
        .unwrap()
    }

    fn mapping_with(naming: RootNaming, bucket: &str, object: &str) -> PathMapping {
        let (Ok(bucket), Ok(object)) = (BucketName::new(bucket), ObjectName::new(object)) else {
            return PathMapping::Unrepresentable(Reason::InvalidName);
        };
        name_to_path(&bucket, &object, naming)
    }

    fn mapping(bucket: &str, object: &str) -> PathMapping {
        mapping_with(RootNaming::posix(), bucket, object)
    }

    fn decode_with(naming: RootNaming, path: &Path) -> Result<(String, String), Reason> {
        path_to_name(path, naming)
            .map(|(bucket, object)| (bucket.as_str().to_owned(), object.as_str().to_owned()))
    }

    fn decode(path: &Path) -> Result<(String, String), Reason> {
        decode_with(RootNaming::posix(), path)
    }

    #[test]
    fn literal_names_round_trip_without_normalization() {
        // Windows keeps its own component rules whatever the root's rules say, and they refuse a
        // colon, so the colon names round-trip only off Windows.
        #[cfg(not(windows))]
        let colon_names = ["a:b", "logs/2026-10-28T12:00:00.log"];
        #[cfg(windows)]
        let colon_names: [&str; 0] = [];
        let plain_names = [
            "%2F/%25.txt",
            "日本語/é/e\u{301}",
            "._assets/ok",
            "dir.swp/ok",
            "4913/ok",
        ];
        for object in plain_names.into_iter().chain(colon_names) {
            let PathMapping::File(path) = mapping("demo-bucket", object) else {
                panic!("representable lexical name: {object}");
            };
            assert_eq!(
                decode(&path).unwrap(),
                ("demo-bucket".into(), object.into())
            );
        }
        for object in [r"a\b", r"logs\2026-10-28.log"] {
            assert_eq!(
                mapping("demo-bucket", object),
                PathMapping::Unrepresentable(Reason::HostRestriction),
                "{object:?}"
            );
        }
        // On Windows a backslash separates components, so this path is another object's.
        #[cfg(not(windows))]
        assert_eq!(
            path_to_name(Path::new(r"demo-bucket/a\b"), RootNaming::posix()),
            Err(Reason::HostRestriction)
        );
    }

    // Windows refuses a colon and its device names by its own component rules, whatever the
    // root's rules say; `windows_restrictions_cover_devices_and_special_components` covers them.
    #[test]
    #[cfg(not(windows))]
    fn colon_names_follow_the_root_rules_in_both_directions() {
        let rejecting = naming(Colon::Rejected, Some(255));
        for object in ["a:b", "logs/2026-10-28T12:00:00.log", "dir:x/file"] {
            assert_eq!(
                mapping_with(rejecting, "demo-bucket", object),
                PathMapping::Unrepresentable(Reason::HostRestriction),
                "{object:?}"
            );
            let PathMapping::File(path) = mapping("demo-bucket", object) else {
                panic!("a colon maps where the root stores one: {object}");
            };
            assert_eq!(decode_with(rejecting, &path), Err(Reason::HostRestriction));
        }
        // A refused colon brings in no other host's restrictions.
        for object in ["CON", "a?b", "a ", "a.", "a|b", "a*b"] {
            assert!(
                matches!(
                    mapping_with(rejecting, "demo-bucket", object),
                    PathMapping::File(_)
                ),
                "{object:?}"
            );
        }
    }

    #[test]
    fn mapping_preserves_the_api_name_byte_limit() {
        let bucket = BucketName::new("demo-bucket").unwrap();
        // The API's longest name, 1024 bytes, maps when each component fits the root.
        let object = ObjectName::new(
            [
                "é".repeat(127),
                "é".repeat(127),
                "é".repeat(127),
                "é".repeat(127),
                "x".repeat(4),
            ]
            .join("/"),
        )
        .unwrap();
        assert_eq!(object.as_str().len(), 1024);
        let PathMapping::File(path) = name_to_path(&bucket, &object, RootNaming::posix()) else {
            panic!("valid API-sized name");
        };
        assert_eq!(
            path_to_name(&path, RootNaming::posix()),
            Ok((bucket.clone(), object))
        );
        // One component over the root's limit is the root's restriction, not an invalid name.
        let long = ObjectName::new("é".repeat(128)).unwrap();
        assert_eq!(
            name_to_path(&bucket, &long, RootNaming::posix()),
            PathMapping::Unrepresentable(Reason::HostRestriction)
        );
        assert_eq!(
            name_to_path(&bucket, &long, naming(Colon::Accepted, None)),
            PathMapping::Unrepresentable(Reason::HostRestriction)
        );
        let short = ObjectName::new("x".repeat(9)).unwrap();
        assert_eq!(
            name_to_path(&bucket, &short, naming(Colon::Accepted, Some(8))),
            PathMapping::Unrepresentable(Reason::HostRestriction)
        );
        for object in [
            "é".repeat(513),
            "x".repeat(1025),
            "a\nb".into(),
            "a\rb".into(),
        ] {
            assert!(ObjectName::new(object).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_rejected_without_lossy_conversion() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = Path::new(OsStr::from_bytes(b"demo-bucket/\xff"));
        assert_eq!(
            path_to_name(path, RootNaming::posix()),
            Err(Reason::InvalidName)
        );
    }

    #[test]
    fn unsafe_reserved_and_ignored_names_are_explicit() {
        for object in ["", "/a", "a/", "a//b", "a/./b", "a/../b", "a\0b", ".", ".."] {
            assert_eq!(
                mapping("demo-bucket", object),
                PathMapping::Unrepresentable(Reason::InvalidName)
            );
        }
        for component in [MARKER, MARKER_STAGING, WORKSPACE] {
            for object in [component.to_owned(), format!("a/{component}/b")] {
                assert_eq!(
                    mapping("demo-bucket", &object),
                    PathMapping::Unrepresentable(Reason::Reserved)
                );
            }
        }
        for leaf in [".DS_Store", "._resource", "4913", "file.swp", "file~"] {
            assert_eq!(
                mapping("demo-bucket", &format!("a/{leaf}")),
                PathMapping::Unrepresentable(Reason::Ignored)
            );
        }
        for raw in [
            "/demo-bucket/a",
            "demo-bucket//a",
            "demo-bucket/./a",
            "demo-bucket/a/",
            "demo-bucket/a/../b",
        ] {
            assert!(decode(Path::new(raw)).is_err());
        }
        assert!(matches!(
            mapping("../bucket", "object"),
            PathMapping::Unrepresentable(Reason::InvalidName)
        ));
    }

    #[test]
    #[cfg(windows)]
    fn windows_restrictions_cover_devices_and_special_components() {
        for component in [
            "CON", "con.txt", "COM1", "LPT9.log", "COM¹", "a:", "a\\b", "a?b", "a ", "a.",
            "a\u{1}b",
        ] {
            assert!(windows_component_is_invalid(component), "{component:?}");
        }
        for component in ["COM0", "COM10", "LPT0", "console", "%2F", "é", "a b"] {
            assert!(!windows_component_is_invalid(component), "{component:?}");
        }
    }

    #[cfg(not(windows))]
    fn observations() -> impl Strategy<Value = Observation> {
        (
            prop_oneof![Just(Colon::Accepted), Just(Colon::Rejected)],
            prop_oneof![Just(Case::Sensitive), Just(Case::Insensitive)],
            proptest::option::of(1usize..=300),
        )
            .prop_map(|(colon, case, name_max)| Observation {
                colon,
                case,
                spelling: Spelling::Preserved,
                name_max: NameMax::from(name_max),
            })
    }

    proptest! {
        #[test]
        fn arbitrary_names_round_trip_or_are_explicitly_rejected(object in ".{0,1100}") {
            if let PathMapping::File(path) = mapping("demo-bucket", &object) {
                prop_assert_eq!(decode(&path), Ok(("demo-bucket".to_owned(), object)));
                prop_assert!(path.is_relative());
            }
        }

        #[test]
        fn arbitrary_paths_never_clean_into_another_name(
            bucket in "bucket-[a-f0-9]{1,12}",
            components in proptest::collection::vec("file-[a-zA-Z0-9_%-]{1,20}", 1..8),
        ) {
            let raw = format!("{bucket}{}{}", std::path::MAIN_SEPARATOR, components.join(std::path::MAIN_SEPARATOR_STR));
            let path = Path::new(&raw);
            let naming = RootNaming::posix();
            let (parsed_bucket, object) = path_to_name(path, naming).unwrap();
            prop_assert_eq!(parsed_bucket.as_str(), bucket);
            prop_assert_eq!(object.as_str(), components.join("/"));
            let PathMapping::File(mapped) = name_to_path(&parsed_bucket, &object, naming) else {
                prop_assert!(false, "inverse accepted an unrepresentable name");
                return Ok(());
            };
            prop_assert_eq!(mapped.as_os_str(), path.as_os_str());
            let malformed = raw.replacen(std::path::MAIN_SEPARATOR, &format!("{0}.{0}", std::path::MAIN_SEPARATOR), 1);
            prop_assert!(path_to_name(Path::new(&malformed), naming).is_err());
        }

        /// One rules value decides a name the same way in both mapping directions, in the
        /// per-component check that parent walks and target checks apply, and in the catalog's
        /// path validator, whatever the root's rules are. A Windows build adds its own fixed
        /// restrictions on top of the root's rules, which
        /// `windows_restrictions_cover_devices_and_special_components` covers instead.
        #[cfg(not(windows))]
        #[test]
        fn one_rules_value_decides_names_in_both_directions_and_the_catalog(
            observed in observations(),
            object in "([a-z:\\\\é]{0,140}/){0,3}[a-z:\\\\é]{1,140}",
        ) {
            let naming = RootNaming::try_from(observed).unwrap();
            let bucket = BucketName::new("demo-bucket").unwrap();
            let Ok(object) = ObjectName::new(object) else {
                return Ok(());
            };
            let components = || std::iter::once(bucket.as_str()).chain(object.as_str().split('/'));
            match name_to_path(&bucket, &object, naming) {
                PathMapping::File(path) => {
                    prop_assert_eq!(path_to_name(&path, naming), Ok((bucket.clone(), object.clone())));
                    for component in &path {
                        prop_assert!(naming.admits(component), "{component:?}");
                    }
                    // The catalog stores `/` between components on every host.
                    let text = path
                        .iter()
                        .map(|component| component.to_str().unwrap())
                        .collect::<Vec<_>>()
                        .join("/");
                    prop_assert!(crate::storage::sync_state::SyncPath::try_from(text).is_ok());
                }
                PathMapping::Unrepresentable(Reason::HostRestriction) => {
                    prop_assert!(components().any(|component| component.contains('\\') || !naming.admits(component.as_ref())));
                }
                // An empty component names no file, whatever the root's rules.
                PathMapping::Unrepresentable(reason) => prop_assert_eq!(reason, Reason::InvalidName),
            }
        }
    }
}
