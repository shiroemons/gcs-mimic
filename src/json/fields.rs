use super::{JsonError, parse_query};
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};

const MAX_LENGTH: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_COUNT: usize = 256;

// Validate against the same pinned Discovery schema as the compatibility inventory,
// including optional fields absent from a particular rendered resource.
#[expect(
    clippy::expect_used,
    reason = "the schemas are compiled in with include_str!, and the field tests parse them"
)]
static SCHEMAS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../compatibility/upstream/json-schemas.json"
    ))
    .expect("checked-in Discovery schemas")
});

#[derive(Clone, Debug, Default)]
pub struct Fields {
    whole: bool,
    children: BTreeMap<String, Fields>,
}

impl Fields {
    pub fn from_query(raw: Option<&str>, schema: &str) -> Result<Self, JsonError> {
        let mut query = parse_query(raw.unwrap_or_default())?;
        Self::take(&mut query, schema)
    }

    pub(crate) fn take(
        query: &mut BTreeMap<String, String>,
        schema: &str,
    ) -> Result<Self, JsonError> {
        match query.remove("fields") {
            Some(value) => Self::parse(&value, schema),
            None => Ok(Self {
                whole: true,
                children: BTreeMap::new(),
            }),
        }
    }

    pub fn parse(value: &str, schema: &str) -> Result<Self, JsonError> {
        let fields = Self::parse_syntax(value)?;
        fields
            .validate(&SCHEMAS["schemas"][schema])
            .map_err(|mut error| {
                error.message = format!("{}: {value}", error.message);
                error
            })?;
        Ok(fields)
    }

    pub(crate) fn parse_syntax(value: &str) -> Result<Self, JsonError> {
        Self::syntax(value).map_err(|mut error| {
            error.message = format!("{}: {value}", error.message);
            error
        })
    }

    fn syntax(value: &str) -> Result<Self, JsonError> {
        if value.len() > MAX_LENGTH {
            return Err(invalid("fields exceeds 4096 bytes"));
        }
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(unsupported("empty or whitespace-containing fields"));
        }
        let mut parser = Parser {
            rest: value,
            count: 0,
        };
        let fields = parser.list(0)?;
        if !parser.rest.is_empty() {
            return Err(invalid("Unbalanced fields parentheses"));
        }
        Ok(fields)
    }

    fn merge(&mut self, other: Self) {
        self.whole |= other.whole;
        for (key, value) in other.children {
            self.children.entry(key).or_default().merge(value);
        }
    }

    fn validate(&self, schema: &Value) -> Result<(), JsonError> {
        let schema = if let Some(name) = schema["$ref"].as_str() {
            &SCHEMAS["schemas"][name]
        } else {
            schema
        };
        if schema["type"] == "array" {
            return self.validate(&schema["items"]);
        }
        for (name, child) in &self.children {
            if name == "*" {
                if !child.whole || !child.children.is_empty() {
                    return Err(unsupported("wildcard sub-selection"));
                }
                if schema["type"] != "object" {
                    return Err(invalid("Invalid field selection *"));
                }
                continue;
            }
            let field = schema["properties"]
                .get(name)
                .or_else(|| schema.get("additionalProperties"))
                .ok_or_else(|| invalid(format!("Invalid field selection {name}")))?;
            child.validate(field)?;
        }
        Ok(())
    }

    pub fn apply(&self, value: &mut Value) {
        if self.whole {
            return;
        }
        match value {
            Value::Array(items) => {
                for item in items {
                    self.apply(item);
                }
            }
            Value::Object(object) => {
                object.retain(|name, value| {
                    match (self.children.get(name), self.children.get("*")) {
                        (_, Some(wildcard)) if wildcard.whole => true,
                        (Some(child), _) => {
                            child.apply(value);
                            true
                        }
                        _ => false,
                    }
                });
            }
            _ => {}
        }
    }
}

fn invalid(message: impl Into<String>) -> JsonError {
    JsonError::invalid_parameter(message)
}

fn unsupported(name: &str) -> JsonError {
    JsonError::new(
        hyper::StatusCode::BAD_REQUEST,
        "badRequest",
        format!(
            "gcs-mimic does not support JSON API selector {name}; omit fields or select a supported field (for example, fields=name)."
        ),
    )
}

struct Parser<'a> {
    rest: &'a str,
    count: usize,
}

impl Parser<'_> {
    fn list(&mut self, depth: usize) -> Result<Fields, JsonError> {
        let mut fields = Fields::default();
        loop {
            fields.merge(self.path(depth)?);
            if !self.rest.starts_with(',') {
                break;
            }
            self.rest = &self.rest[1..];
        }
        Ok(fields)
    }

    fn path(&mut self, depth: usize) -> Result<Fields, JsonError> {
        self.count += 1;
        if depth >= MAX_DEPTH || self.count > MAX_COUNT {
            return Err(invalid("fields exceeds depth 32 or 256 selections"));
        }
        let end = self
            .rest
            .find([',', '/', '(', ')'])
            .unwrap_or(self.rest.len());
        let name = &self.rest[..end];
        if name.is_empty() || (name.contains('*') && name != "*") {
            return Err(invalid("Invalid field selection"));
        }
        self.rest = &self.rest[end..];
        let child = if self.rest.starts_with('/') {
            self.rest = &self.rest[1..];
            self.path(depth + 1)?
        } else if self.rest.starts_with('(') {
            self.rest = &self.rest[1..];
            let child = self.list(depth + 1)?;
            if !self.rest.starts_with(')') {
                return Err(invalid("Unbalanced fields parentheses"));
            }
            self.rest = &self.rest[1..];
            child
        } else {
            Fields {
                whole: true,
                children: BTreeMap::new(),
            }
        };
        Ok(Fields {
            whole: false,
            children: BTreeMap::from([(name.to_owned(), child)]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fields_select_paths_subselectors_wildcards_and_omitted_values() {
        let original = json!({"kind":"storage#objects","items":[
            {"name":"empty","size":"0","metadata":{"key":"value","other":"hidden"}},
            {"name":"plain","size":"2"}
        ],"prefixes":["dir/"],"nextPageToken":"cursor"});
        for selector in [
            "items(name,metadata(key)),nextPageToken",
            "items/name,items/metadata/key,nextPageToken",
        ] {
            let mut value = original.clone();
            Fields::parse(selector, "Objects")
                .unwrap()
                .apply(&mut value);
            assert_eq!(
                value,
                json!({"items":[{"name":"empty","metadata":{"key":"value"}},{"name":"plain"}],"nextPageToken":"cursor"})
            );
        }
        for selector in ["*", "*,items(name)", "items(*),kind,prefixes,nextPageToken"] {
            let mut value = original.clone();
            Fields::parse(selector, "Objects")
                .unwrap()
                .apply(&mut value);
            assert_eq!(value, original);
        }
        let mut value = original.clone();
        Fields::parse("items(metadata/*),prefixes", "Objects")
            .unwrap()
            .apply(&mut value);
        assert_eq!(
            value,
            json!({"items":[{"metadata":{"key":"value","other":"hidden"}},{}],"prefixes":["dir/"]})
        );
        let mut value = json!({"name":"zero","size":"0"});
        Fields::parse("size,metadata/key,contentEncoding", "Object")
            .unwrap()
            .apply(&mut value);
        assert_eq!(value, json!({"size":"0"}));
    }

    #[test]
    fn fields_reject_malformed_unknown_and_scalar_children() {
        for selector in [
            ",name",
            "name,",
            "name,,size",
            "metadata/",
            "/name",
            "metadata//key",
            "metadata()",
            "metadata(key",
            "name)",
            "(name)",
            "name(size)",
            "name/*",
            "n*",
            "unknown",
            "metadata/key/unknown",
            "*,unknown",
        ] {
            let error = Fields::parse(selector, "Object").unwrap_err();
            assert_eq!(
                (error.status.as_u16(), error.reason),
                (400, "invalidParameter"),
                "{selector}"
            );
            assert!(error.message.contains(selector), "{}", error.message);
        }
        for selector in [
            "",
            " name",
            "name, size",
            "metadata( key)",
            "items/*/name",
            "items/*/name,items/*",
            "items/*,items/*/unknown",
        ] {
            let error = Fields::parse(selector, "Objects").unwrap_err();
            assert_eq!(
                (error.status.as_u16(), error.reason),
                (400, "badRequest"),
                "{selector}"
            );
            assert!(
                error
                    .message
                    .starts_with("gcs-mimic does not support JSON API selector")
            );
        }
    }

    #[test]
    fn fields_persisted_selection_does_not_depend_on_current_schema() {
        let selector = "retiredProperty";
        assert!(Fields::parse(selector, "Object").is_err());
        let mut value = json!({"retiredProperty":"completed","name":"hidden"});
        Fields::parse_syntax(selector).unwrap().apply(&mut value);
        assert_eq!(value, json!({"retiredProperty":"completed"}));
        assert!(Fields::parse_syntax("name,").is_err());
    }

    #[test]
    fn fields_rendered_resource_paths_match_pinned_schemas() {
        use crate::{
            config::CompatProfile,
            json,
            storage::{ObjectListCursor, ObjectListPage},
        };

        fn check(value: &Value, path: &str, schema: &str) {
            match value {
                Value::Object(object) => {
                    for (name, value) in object {
                        let path = if path.is_empty() {
                            name.clone()
                        } else {
                            format!("{path}/{name}")
                        };
                        Fields::parse(&path, schema)
                            .unwrap_or_else(|error| panic!("{schema} {path}: {error}"));
                        check(value, &path, schema);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        check(item, path, schema);
                    }
                }
                _ => {}
            }
        }

        let base = "http://localhost";
        let mut bucket = json::tests::record();
        bucket.labels.insert("team".into(), "example".into());
        bucket.versioning_enabled = true;
        bucket.default_event_based_hold = true;
        bucket.metageneration = 2;
        let mut object = json::object_tests::record();
        object.content_encoding = Some("gzip".into());
        object.content_disposition = Some("attachment".into());
        object.content_language = Some("en".into());
        object.cache_control = Some("no-cache".into());
        object.metadata.insert("team".into(), "example".into());
        object.event_based_hold = true;
        object.time_deleted = Some(object.updated);
        let params = json::object_list::parse_list(bucket.name.clone(), "delimiter=/").unwrap();
        let page = ObjectListPage {
            next: Some(ObjectListCursor {
                name: object.name.clone(),
                is_prefix: false,
                generation: Some(object.generation),
            }),
            items: vec![object.clone()],
            prefixes: vec!["dir/".into()],
        };
        let policy = json::parse_policy(br#"{"etag":"AQ==","bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#).unwrap();
        for profile in [CompatProfile::Native, CompatProfile::LegacyEmulator] {
            check(
                &json::bucket_resource_with_profile(&bucket, base, json::Projection::Full, profile)
                    .unwrap(),
                "",
                "Bucket",
            );
            check(
                &json::bucket_list_with_profile(
                    std::slice::from_ref(&bucket),
                    Some("cursor"),
                    base,
                    json::Projection::Full,
                    profile,
                )
                .unwrap(),
                "",
                "Buckets",
            );
        }
        check(
            &json::object_resource(&object, base, json::Projection::Full).unwrap(),
            "",
            "Object",
        );
        check(
            &json::object_list(&page, &params, base).unwrap(),
            "",
            "Objects",
        );
        check(&json::policy_resource(&policy, &bucket.name), "", "Policy");
    }

    #[test]
    fn fields_bound_length_depth_and_selection_count() {
        let exact = format!("metadata/{}", "k".repeat(MAX_LENGTH - 9));
        assert!(Fields::parse(&exact, "Object").is_ok());
        assert_eq!(
            Fields::parse(&(exact + "k"), "Object")
                .unwrap_err()
                .status
                .as_u16(),
            400
        );
        assert!(Fields::parse(&vec!["name"; MAX_COUNT].join(","), "Object").is_ok());
        assert_eq!(
            Fields::parse(&vec!["name"; MAX_COUNT + 1].join(","), "Object")
                .unwrap_err()
                .status
                .as_u16(),
            400
        );
        for separator in ["/", "("] {
            let value = if separator == "/" {
                vec!["a"; MAX_DEPTH].join("/")
            } else {
                format!(
                    "{}a{}",
                    "a(".repeat(MAX_DEPTH - 1),
                    ")".repeat(MAX_DEPTH - 1)
                )
            };
            assert!(
                Parser {
                    rest: &value,
                    count: 0
                }
                .list(0)
                .is_ok()
            );
            let deeper = format!("a/{value}");
            assert!(
                Parser {
                    rest: &deeper,
                    count: 0
                }
                .list(0)
                .is_err()
            );
        }
    }
}
