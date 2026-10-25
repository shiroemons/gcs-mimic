#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use crate::storage::admin::{BucketName, ObjectName};

pub const MARKER: &str = ".gcs-mimic-sync.json";
pub const WORKSPACE: &str = ".gcs-mimic-sync-work";

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

/// Produces a lexical candidate only; callers must check the actual root's
/// aliases, length limits, collisions and no-follow identities before any I/O.
pub fn name_to_path(bucket: &BucketName, object: &ObjectName) -> PathMapping {
    match candidate(bucket, object) {
        Ok(path) => PathMapping::File(path),
        Err(reason) => PathMapping::Unrepresentable(reason),
    }
}

fn candidate(bucket: &BucketName, object: &ObjectName) -> Result<PathBuf, Reason> {
    let (bucket, object) = (bucket.as_str(), object.as_str());
    let mut path = PathBuf::new();
    for component in std::iter::once(bucket).chain(object.split('/')) {
        if component.is_empty() || matches!(component, "." | "..") || component.contains('\0') {
            return Err(Reason::InvalidName);
        }
        if matches!(component, MARKER | WORKSPACE) {
            return Err(Reason::Reserved);
        }
        if cfg!(windows) && windows_component_is_invalid(component) {
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

/// Decodes only the exact relative path spelling emitted by `name_to_path`.
pub fn path_to_name(path: &Path) -> Result<(BucketName, ObjectName), Reason> {
    let raw = path.to_str().ok_or(Reason::InvalidName)?;
    let text = if cfg!(windows) {
        raw.replace('\\', "/")
    } else {
        raw.to_owned()
    };
    let (bucket, object) = text.split_once('/').ok_or(Reason::InvalidName)?;
    let bucket = BucketName::new(bucket).map_err(|_| Reason::InvalidName)?;
    let object = ObjectName::new(object).map_err(|_| Reason::InvalidName)?;
    let candidate = candidate(&bucket, &object)?;
    if candidate.as_os_str() != path.as_os_str() {
        return Err(Reason::InvalidName);
    }
    Ok((bucket, object))
}

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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn mapping(bucket: &str, object: &str) -> PathMapping {
        let (Ok(bucket), Ok(object)) = (BucketName::new(bucket), ObjectName::new(object)) else {
            return PathMapping::Unrepresentable(Reason::InvalidName);
        };
        name_to_path(&bucket, &object)
    }

    fn decode(path: &Path) -> Result<(String, String), Reason> {
        path_to_name(path)
            .map(|(bucket, object)| (bucket.as_str().to_owned(), object.as_str().to_owned()))
    }

    #[test]
    fn literal_names_round_trip_without_normalization() {
        for object in [
            "%2F/%25.txt",
            "日本語/é/e\u{301}",
            "._assets/ok",
            "dir.swp/ok",
            "4913/ok",
        ] {
            let PathMapping::File(path) = mapping("demo-bucket", object) else {
                panic!("representable lexical name: {object}");
            };
            assert_eq!(
                decode(&path).unwrap(),
                ("demo-bucket".into(), object.into())
            );
        }
        let mapping = mapping("demo-bucket", r"a\b");
        if cfg!(windows) {
            assert_eq!(
                mapping,
                PathMapping::Unrepresentable(Reason::HostRestriction)
            );
        } else {
            let PathMapping::File(path) = mapping else {
                panic!("Unix literal backslash");
            };
            assert_eq!(decode(&path).unwrap().1, r"a\b");
        }
    }

    #[test]
    fn unsafe_reserved_and_ignored_names_are_explicit() {
        for object in ["", "/a", "a/", "a//b", "a/./b", "a/../b", "a\0b", ".", ".."] {
            assert_eq!(
                mapping("demo-bucket", object),
                PathMapping::Unrepresentable(Reason::InvalidName)
            );
        }
        for component in [MARKER, WORKSPACE] {
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
            assert!(path_to_name(Path::new(raw)).is_err());
        }
        assert!(matches!(
            mapping("../bucket", "object"),
            PathMapping::Unrepresentable(Reason::InvalidName)
        ));
    }

    #[test]
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
            let (parsed_bucket, object) = path_to_name(path).unwrap();
            prop_assert_eq!(parsed_bucket.as_str(), bucket);
            prop_assert_eq!(object.as_str(), components.join("/"));
            let PathMapping::File(mapped) = name_to_path(&parsed_bucket, &object) else {
                prop_assert!(false, "inverse accepted an unrepresentable name");
                return Ok(());
            };
            prop_assert_eq!(mapped.as_os_str(), path.as_os_str());
            let malformed = raw.replacen(std::path::MAIN_SEPARATOR, &format!("{0}.{0}", std::path::MAIN_SEPARATOR), 1);
            prop_assert!(path_to_name(Path::new(&malformed)).is_err());
        }
    }
}
