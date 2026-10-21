fn version(value: &str) -> Option<(u64, u64, u64)> {
    let parsed = crate::dependencies::version(value)?;
    (format!("{}.{}.{}", parsed.0, parsed.1, parsed.2) == value).then_some(parsed)
}

pub fn tags(current: &str, releases: &[String]) -> Result<Vec<String>, String> {
    let selected = version(current).ok_or("container releases require stable MAJOR.MINOR.PATCH")?;
    let newer: Vec<_> = releases
        .iter()
        .filter_map(|release| version(release))
        .filter(|release| *release > selected)
        .collect();
    let mut tags = vec![current.to_owned()];
    if !newer.iter().any(|v| (v.0, v.1) == (selected.0, selected.1)) {
        tags.push(format!("{}.{}", selected.0, selected.1));
    }
    if newer.is_empty() {
        tags.push("latest".into());
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_never_move_backwards_and_zero_major_is_not_an_alias() {
        assert_eq!(tags("0.2.3", &[]).unwrap(), ["0.2.3", "0.2", "latest"]);
        assert_eq!(tags("0.2.3", &["0.2.10".into()]).unwrap(), ["0.2.3"]);
        assert_eq!(
            tags("0.2.3", &["0.3.0".into(), "1.0.0-rc.1".into()]).unwrap(),
            ["0.2.3", "0.2"]
        );
        assert_eq!(
            tags("1.10.0", &["1.9.9".into(), "1.10.0".into()]).unwrap(),
            ["1.10.0", "1.10", "latest"]
        );
    }

    #[test]
    fn malformed_and_prerelease_versions_cannot_publish() {
        for value in [
            "01.2.3",
            "1.2",
            "v1.2.3",
            "1.2.3-rc.1",
            "1.2.3+build",
            "1.2.3\n",
        ] {
            assert!(tags(value, &[]).is_err(), "{value}");
        }
    }
}
