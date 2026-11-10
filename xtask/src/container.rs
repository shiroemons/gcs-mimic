use crate::release::{InvalidVersion, version::Version};

pub fn tags(current: &str, releases: &[String]) -> Result<Vec<String>, InvalidVersion> {
    let selected: Version = current.parse()?;
    let mut tags = vec![selected.to_string()];
    if selected.is_candidate() {
        return Ok(tags);
    }
    let newer: Vec<_> = releases
        .iter()
        .filter_map(|release| release.parse::<Version>().ok())
        .filter(|release| !release.is_candidate() && *release > selected)
        .collect();
    if !newer.iter().any(|version| version.same_minor(&selected)) {
        tags.push(selected.minor_alias());
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
    fn candidates_publish_only_the_exact_tag_and_never_affect_stable_aliases() {
        for current in ["1.0.0-rc.1", "1.0.0-rc.2"] {
            assert_eq!(
                tags(current, &["0.17.1".into(), "1.0.0-rc.1".into()]).unwrap(),
                [current]
            );
        }
        let candidates = [
            "1.0.0-rc.1".into(),
            "1.0.0-rc.2".into(),
            "1.0.1-rc.1".into(),
        ];
        assert_eq!(
            tags("0.17.1", &candidates).unwrap(),
            ["0.17.1", "0.17", "latest"]
        );
        assert_eq!(
            tags("1.0.0", &candidates).unwrap(),
            ["1.0.0", "1.0", "latest"]
        );
    }

    #[test]
    fn malformed_and_unsupported_prerelease_versions_cannot_publish() {
        for value in [
            "01.2.3",
            "1.2",
            "v1.2.3",
            "1.2.3-beta.1",
            "1.2.3-rc.0",
            "1.2.3+build",
            "1.2.3\n",
        ] {
            assert!(tags(value, &[]).is_err(), "{value}");
        }
    }
}
