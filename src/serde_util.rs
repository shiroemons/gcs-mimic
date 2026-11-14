macro_rules! serialize_as_str {
    ($name:ty) => {
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
    };
}

pub(crate) use serialize_as_str;

/// A JSON object of string values in the map's key order, as `serde_json::to_value` renders a
/// `BTreeMap<String, String>`, without its fallible conversion.
pub(crate) fn string_map(map: &std::collections::BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        map.iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::from(value.as_str())))
            .collect(),
    )
}

/// Discovery int64 fields accept either a JSON integer or a decimal string.
pub(crate) fn int64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    #[test]
    fn string_maps_render_as_serde_renders_them() {
        let map = BTreeMap::from([
            ("b".to_owned(), "2".to_owned()),
            ("a".to_owned(), "\"quoted\" \u{e9}".to_owned()),
            (String::new(), String::new()),
        ]);
        let rendered = super::string_map(&map);
        assert_eq!(rendered, serde_json::to_value(&map).unwrap());
        assert_eq!(rendered.to_string(), serde_json::to_string(&map).unwrap());
    }
}
