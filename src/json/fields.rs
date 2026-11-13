use super::{JsonError, parse_query};
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};

/// A query string holds no more than this, so it also bounds the selections: each takes two bytes.
const MAX_LENGTH: usize = super::MAX_QUERY_BYTES;
const MAX_DEPTH: usize = 32;

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
pub(crate) struct Fields {
    whole: bool,
    children: BTreeMap<String, Fields>,
}

impl Fields {
    pub(crate) fn from_query(raw: Option<&str>, schema: &str) -> Result<Self, JsonError> {
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

    pub(crate) fn parse(value: &str, schema: &str) -> Result<Self, JsonError> {
        let fields = Self::parse_syntax(value)?;
        let schema = SCHEMAS
            .get("schemas")
            .and_then(|schemas| schemas.get(schema))
            .unwrap_or(&Value::Null);
        fields.validate(schema, value).map_err(|mut error| {
            // Cloud Storage named only the rejected field, at the `fields` parameter
            // (observed 2026-10-31 for retentionExpireTime).
            error.location = Some(&super::error::FIELDS_PARAMETER);
            error
        })?;
        Ok(fields)
    }

    /// Parses a selection without a schema. A resumable upload stores the selection it was
    /// started with and parses it again here on completion, so this accepts exactly what
    /// `parse` accepted at initiation, even if the schema has changed since.
    pub(crate) fn parse_syntax(value: &str) -> Result<Self, JsonError> {
        // An empty selection selects nothing, unlike an absent `fields` parameter. Cloud
        // Storage answered a whitespace selection with `{}` on objects, buckets, the bucket
        // list and the IAM policy (cases obj-fields-selectors and bkt-edges-fields); the empty
        // string itself was recorded only for objects.
        if value.is_empty() {
            return Ok(Self::default());
        }
        Self::syntax(value).map_err(|mut error| {
            // The FieldMask wording already quotes the whole expression, and Cloud Storage
            // follows the selection wording with the expression without a colon ("Invalid field
            // selection metadata/", case obj-fields-selectors).
            if error.message == INVALID_SELECTION {
                error.message = format!("{INVALID_SELECTION} {value}");
            } else if error.kind != super::ErrorKind::FieldMaskSyntax {
                error.message = format!("{}: {value}", error.message);
            }
            error
        })
    }

    fn syntax(value: &str) -> Result<Self, JsonError> {
        if value.len() > MAX_LENGTH {
            return Err(invalid("fields exceeds 16 KiB"));
        }
        let mut parser = Parser {
            source: value,
            rest: value,
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

    /// Checks the selection against `schema`. `source` is the whole expression, which Cloud
    /// Storage quotes after a wildcard that has a child.
    fn validate(&self, schema: &Value, source: &str) -> Result<(), JsonError> {
        let schema = if let Some(name) = schema["$ref"].as_str() {
            SCHEMAS
                .get("schemas")
                .and_then(|schemas| schemas.get(name))
                .unwrap_or(&Value::Null)
        } else {
            schema
        };
        if schema["type"] == "array" {
            return self.validate(&schema["items"], source);
        }
        for (name, child) in &self.children {
            if name == "*" {
                if !child.whole || !child.children.is_empty() {
                    return Err(invalid(format!("{NESTED_WILDCARD} {source}")));
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
            child.validate(field, source)?;
        }
        Ok(())
    }

    pub(crate) fn apply(&self, value: &mut Value) {
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
                            // A selection that matches nothing inside an object drops the
                            // object itself rather than leaving `{}`.
                            child.whole || value.as_object().is_none_or(|object| !object.is_empty())
                        }
                        _ => false,
                    }
                });
            }
            _ => {}
        }
    }
}

const INVALID_SELECTION: &str = "Invalid field selection";
/// Cloud Storage's rejection of a wildcard with a child, such as `*/name`, which it follows
/// with the whole expression (case bkt-edges-fields).
const NESTED_WILDCARD: &str = "Leading and nested wildcards are unsupported in field selection:";

fn invalid(message: impl Into<String>) -> JsonError {
    JsonError::invalid_parameter(message).at_parameter("fields")
}

/// A selector parser. Cloud Storage ignored an empty segment such as the middle of
/// `name,,size` or the inside of `metadata()`, and whitespace around a name, so `fields=%20`
/// selected nothing and `name , size` selected both fields, on objects (case
/// obj-fields-selectors) and on buckets, the bucket list and the IAM policy (case
/// bkt-edges-fields).
struct Parser<'a> {
    /// The whole expression, which Cloud Storage quotes when a parenthesis is unmatched.
    source: &'a str,
    rest: &'a str,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start();
    }

    fn list(&mut self, depth: usize) -> Result<Fields, JsonError> {
        let mut fields = Fields::default();
        loop {
            self.skip_whitespace();
            let empty = self.rest.starts_with(',')
                || (depth == 0 && self.rest.is_empty())
                || (depth > 0 && self.rest.starts_with(')'));
            if !empty {
                fields.merge(self.path(depth)?);
                self.skip_whitespace();
            }
            let Some(rest) = self.rest.strip_prefix(',') else {
                break;
            };
            self.rest = rest;
        }
        Ok(fields)
    }

    fn path(&mut self, depth: usize) -> Result<Fields, JsonError> {
        if depth >= MAX_DEPTH {
            return Err(invalid("fields exceeds depth 32"));
        }
        let end = self
            .rest
            .find([',', '/', '(', ')'])
            .unwrap_or(self.rest.len());
        let (name, rest) = self
            .rest
            .split_at_checked(end)
            .ok_or_else(|| invalid(INVALID_SELECTION))?;
        // Whitespace is ignored only around a name: Cloud Storage rejected `na me` as an
        // invalid selection (case bkt-edges-fields).
        let name = name.trim();
        if name.is_empty()
            || name.contains(char::is_whitespace)
            || (name.contains('*') && name != "*")
        {
            return Err(invalid(INVALID_SELECTION));
        }
        self.rest = rest;
        let child = if let Some(rest) = self.rest.strip_prefix('/') {
            self.rest = rest;
            self.path(depth + 1)?
        } else if let Some(rest) = self.rest.strip_prefix('(') {
            self.rest = rest;
            let child = self.list(depth + 1)?;
            let Some(rest) = self.rest.strip_prefix(')') else {
                return Err(JsonError::field_mask_syntax(format!(
                    "Invalid FieldMask '{}'. Cannot find matching ')' for all '('.",
                    self.source
                )));
            };
            self.rest = rest;
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
        let mut value = original;
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
            "metadata/",
            "/name",
            "metadata//key",
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
            // Syntax errors quote the selector; a rejected name is reported alone, at `fields`.
            assert!(
                error.message.contains(selector)
                    || error.location == Some(&crate::json::error::FIELDS_PARAMETER),
                "{}",
                error.message
            );
        }
        // A wildcard with a child is rejected with the whole expression (case bkt-edges-fields).
        for (selector, schema) in [
            ("*/name", "Bucket"),
            ("items/*/name", "Objects"),
            ("items/*/name,items/*", "Objects"),
            ("items/*,items/*/unknown", "Objects"),
        ] {
            let error = Fields::parse(selector, schema).unwrap_err();
            assert_eq!(
                (error.status.as_u16(), error.reason),
                (400, "invalidParameter"),
                "{selector}"
            );
            assert_eq!(
                error.message,
                format!(
                    "Leading and nested wildcards are unsupported in field selection: {selector}"
                )
            );
            assert_eq!(error.location, Some(&crate::json::error::FIELDS_PARAMETER));
        }
        // Whitespace inside a name is an invalid selection where whitespace around one is
        // ignored (case bkt-edges-fields).
        for schema in ["Object", "Bucket"] {
            let error = Fields::parse("na me", schema).unwrap_err();
            assert_eq!(
                (error.status.as_u16(), error.reason, error.message.as_str()),
                (400, "invalidParameter", "Invalid field selection na me")
            );
            assert_eq!(error.location, Some(&crate::json::error::FIELDS_PARAMETER));
        }
        assert_eq!(
            Fields::parse("metadata/", "Object").unwrap_err().message,
            "Invalid field selection metadata/"
        );
    }

    #[test]
    fn empty_selections_and_empty_segments_follow_cloud_storage() {
        let original = json!({"name":"o","size":"5","metadata":{"a":"1"}});
        let mut value = original.clone();
        Fields::parse("", "Object").unwrap().apply(&mut value);
        assert_eq!(value, json!({}));
        let mut value = original.clone();
        Fields::parse("name,,size", "Object")
            .unwrap()
            .apply(&mut value);
        assert_eq!(value, json!({"name":"o","size":"5"}));
        for (selector, expected) in [
            (" ", json!({})),
            ("name , size", json!({"name":"o","size":"5"})),
            (" metadata( a ) ", json!({"metadata":{"a":"1"}})),
            ("metadata()", json!({})),
        ] {
            let mut value = original.clone();
            Fields::parse(selector, "Object").unwrap().apply(&mut value);
            assert_eq!(value, expected, "{selector:?}");
        }
        // Buckets, the bucket list and the IAM policy answered the same selectors alike
        // (case bkt-edges-fields).
        let bucket = json!({"name":"b","location":"US-CENTRAL1","labels":{"a":"1"}});
        for (selector, schema, expected) in [
            (" ", "Bucket", json!({})),
            (
                "name , location",
                "Bucket",
                json!({"name":"b","location":"US-CENTRAL1"}),
            ),
            (
                "name,,location",
                "Bucket",
                json!({"name":"b","location":"US-CENTRAL1"}),
            ),
            ("labels()", "Bucket", json!({})),
            (" ", "Buckets", json!({})),
            (" ", "Policy", json!({})),
        ] {
            let mut value = bucket.clone();
            Fields::parse(selector, schema).unwrap().apply(&mut value);
            assert_eq!(value, expected, "{schema} {selector:?}");
        }
        // A selection that matches nothing inside metadata drops the key instead of `{}`.
        let mut value = original;
        Fields::parse("name,metadata/zzz", "Object")
            .unwrap()
            .apply(&mut value);
        assert_eq!(value, json!({"name":"o"}));
        let error = Fields::parse("metadata(a", "Object").unwrap_err();
        assert_eq!(error.kind, super::super::ErrorKind::FieldMaskSyntax);
        assert_eq!(
            error.message,
            "Invalid FieldMask 'metadata(a'. Cannot find matching ')' for all '('."
        );
    }

    #[test]
    fn fields_persisted_selection_does_not_depend_on_current_schema() {
        let selector = "retiredProperty";
        assert!(Fields::parse(selector, "Object").is_err());
        let mut value = json!({"retiredProperty":"completed","name":"hidden"});
        Fields::parse_syntax(selector).unwrap().apply(&mut value);
        assert_eq!(value, json!({"retiredProperty":"completed"}));
        assert!(Fields::parse_syntax("name(").is_err());
        // A resumable completion parses the stored selection again, so every selection the
        // initiation accepted must parse here too, empty and whitespace ones included.
        for (selector, expected) in [
            ("", json!({})),
            (" ", json!({})),
            ("name , size", json!({"name":"o","size":"5"})),
            ("name,,size", json!({"name":"o","size":"5"})),
        ] {
            assert!(Fields::parse(selector, "Object").is_ok(), "{selector:?}");
            let mut value = json!({"name":"o","size":"5","bucket":"b"});
            Fields::parse_syntax(selector).unwrap().apply(&mut value);
            assert_eq!(value, expected, "{selector:?}");
        }
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
        object.event_based_hold = crate::object::Hold::Held;
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
            bucket_metageneration: None,
        };
        let policy = json::parse_policy(br#"{"etag":"AQ==","bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#).unwrap();
        for profile in [CompatProfile::Native, CompatProfile::FakeGcsServer] {
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
        assert_eq!(exact.len(), crate::json::MAX_QUERY_BYTES);
        assert!(Fields::parse(&exact, "Object").is_ok());
        assert_eq!(
            Fields::parse(&(exact + "k"), "Object")
                .unwrap_err()
                .status
                .as_u16(),
            400
        );
        // The length alone bounds the selections, far above the former limit of 256.
        let selections = "name,".repeat(MAX_LENGTH / 5 - 1) + "name";
        assert!(selections.len() <= MAX_LENGTH);
        assert!(Fields::parse(&selections, "Object").is_ok());
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
                    source: &value,
                    rest: &value,
                }
                .list(0)
                .is_ok()
            );
            let deeper = format!("a/{value}");
            assert!(
                Parser {
                    source: &deeper,
                    rest: &deeper,
                }
                .list(0)
                .is_err()
            );
        }
    }
}
