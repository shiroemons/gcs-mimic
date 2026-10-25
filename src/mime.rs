//! Fixed source-seed MIME profile; provenance and notices live in mime.NOTICE.

use std::{collections::BTreeMap, str::FromStr, sync::LazyLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedContentType {
    OctetStream,
    Extension,
}

impl FromStr for SeedContentType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "octet-stream" => Ok(Self::OctetStream),
            "extension" => Ok(Self::Extension),
            _ => Err("seed_content_type must be octet-stream or extension".into()),
        }
    }
}

static TYPES: LazyLock<BTreeMap<&str, &str>> = LazyLock::new(|| {
    include_str!("mime.tsv")
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect()
});

impl SeedContentType {
    pub fn for_name(self, name: &str) -> &'static str {
        if self == Self::Extension {
            let leaf = name.rsplit('/').next().unwrap_or(name);
            if let Some(dot) = leaf.rfind('.') {
                let extension = &leaf[dot..];
                if let Some(value) = TYPES.get(extension) {
                    return value;
                }
                // Only these non-ASCII Go Unicode 17.0 simple-lowercase mappings can reach table keys.
                let lower: String = extension
                    .chars()
                    .map(|c| match c {
                        '\u{0130}' => 'i',
                        '\u{212a}' => 'k',
                        c => c.to_ascii_lowercase(),
                    })
                    .collect();
                if let Some(value) = TYPES.get(lower.as_str()) {
                    return value;
                }
            }
        }
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_go_suffix_case_and_media_types() {
        for (name, expected) in [
            ("note.txt", "text/plain; charset=utf-8"),
            ("NOTE.TXT", "text/plain; charset=utf-8"),
            (".yaml", "application/x-yaml"),
            ("archive.tar.gz", "application/gzip"),
            ("foo.", "application/octet-stream"),
            ("dir.txt/payload", "application/octet-stream"),
            ("payload.unknown-extension", "application/octet-stream"),
            ("style.css", "text/css; charset=utf-8"),
            ("data.json", "application/json"),
            ("document.SGML", "text/sgml; charset=utf-8"),
            ("script.dms", "text/vnd.dmclientscript; charset=utf-8"),
            ("calibration.a2l", "application/A2L"),
            ("config.yml", "application/x-yaml"),
            ("x.KML", "application/vnd.google-earth.kml+xml"),
            ("x.GİF", "image/gif"),
            ("x.G³", "application/vnd.geocube+xml"),
            ("x.TEICORPUS", "application/tei+xml"),
        ] {
            assert_eq!(
                SeedContentType::Extension.for_name(name),
                expected,
                "{name}"
            );
            assert_eq!(
                SeedContentType::OctetStream.for_name(name),
                "application/octet-stream"
            );
        }
        assert_eq!(TYPES.len(), 1406);
        assert!(
            TYPES
                .keys()
                .all(|key| key.chars().all(|c| c.is_ascii() || c == '³'))
        );
        assert_eq!(include_str!("mime.tsv").lines().count(), TYPES.len());
    }
}
