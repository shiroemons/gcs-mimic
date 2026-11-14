use crate::object::ObjectTarget;
use serde_json::{Map, Value};

crate::storage::opaque_id::opaque_id! {
    /// An unguessable database key: its authoritative request binding stays in the catalog.
    pub(crate) struct RewriteToken([u8; 32]);
    invalid = "invalid rewrite token";
    column = text;
}

impl RewriteToken {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        crate::storage::opaque_id::random().map(Self)
    }
}

/// Neither name carries a version: the source parameter or token selects one, and
/// publication assigns the destination's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RewriteNames {
    pub(crate) source: ObjectTarget<()>,
    pub(crate) destination: ObjectTarget<()>,
}

impl RewriteNames {
    /// Adds the source version a call reads. There is no reverse conversion, so a version
    /// can never be passed in and then ignored.
    pub(crate) fn source_at(&self, generation: Option<i64>) -> ObjectTarget {
        ObjectTarget::new(&self.source.bucket, &self.source.name, generation)
    }
}

/// Includes the selected immutable generation, even when sourceGeneration was omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RewriteBinding {
    pub source_bucket: String,
    pub source_object: String,
    pub source_generation: i64,
    pub destination_bucket: String,
    pub destination_object: String,
    pub parameters: Map<String, Value>,
    pub metadata: Option<Value>,
}

impl RewriteBinding {
    /// Continuations may omit optional fields, but cannot replace stored conditions or metadata.
    pub(crate) fn accepts(
        &self,
        names: &RewriteNames,
        supplied_parameters: &Map<String, Value>,
        supplied_metadata: Option<&Value>,
    ) -> bool {
        names.source.bucket == self.source_bucket
            && names.source.name == self.source_object
            && names.destination.bucket == self.destination_bucket
            && names.destination.name == self.destination_object
            && supplied_parameters
                .iter()
                .all(|(key, value)| self.parameters.get(key) == Some(value))
            && supplied_metadata.is_none_or(|value| self.metadata.as_ref() == Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tokens_are_opaque_and_tampering_never_selects_the_original_token() {
        let token = RewriteToken::generate().unwrap();
        let text = token.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<RewriteToken>(), Ok(token));
        assert_ne!(token, RewriteToken::generate().unwrap());
        for index in 0..64 {
            let mut tampered = text.as_bytes().to_vec();
            tampered[index] = if tampered[index] == b'0' { b'1' } else { b'0' };
            assert_ne!(
                std::str::from_utf8(&tampered)
                    .unwrap()
                    .parse::<RewriteToken>()
                    .ok(),
                Some(token)
            );
        }
        for invalid in [
            "",
            "../object",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64),
        ] {
            assert!(invalid.parse::<RewriteToken>().is_err());
        }
    }

    #[test]
    fn continuation_binds_every_supplied_parameter_body_and_path() {
        let parameters = json!({
            "sourceGeneration": "7", "ifSourceGenerationMatch": "7",
            "ifSourceGenerationNotMatch": "8", "ifSourceMetagenerationMatch": "1",
            "ifSourceMetagenerationNotMatch": "2", "ifGenerationMatch": "0",
            "ifGenerationNotMatch": "9", "ifMetagenerationMatch": "1",
            "ifMetagenerationNotMatch": "2", "maxBytesRewrittenPerCall": "1048576",
            "projection": "noAcl", "fields": "done,resource", "destinationKmsKeyName": "key",
            "destinationPredefinedAcl": "private", "dropContextGroups": "custom",
            "userProject": "project"
        })
        .as_object()
        .unwrap()
        .clone();
        let body = json!({"metadata":{"key":"value"},"storageClass":"NEARLINE"});
        let binding = RewriteBinding {
            source_bucket: "source".into(),
            source_object: "a/b%日本".into(),
            source_generation: 7,
            destination_bucket: "destination".into(),
            destination_object: "c/d".into(),
            parameters: parameters.clone(),
            metadata: Some(body.clone()),
        };
        let names = RewriteNames {
            source: ObjectTarget::new("source", "a/b%日本", ()),
            destination: ObjectTarget::new("destination", "c/d", ()),
        };
        assert!(binding.accepts(&names, &parameters, Some(&body)));
        assert!(binding.accepts(&names, &Map::new(), None));
        for key in parameters.keys() {
            let mut changed = parameters.clone();
            changed.insert(key.clone(), json!("different"));
            assert!(!binding.accepts(&names, &changed, None), "{key}");
        }
        let edits: [fn(&mut RewriteNames); 4] = [
            |names| names.source.bucket = "different".into(),
            |names| names.source.name = "different".into(),
            |names| names.destination.bucket = "different".into(),
            |names| names.destination.name = "different".into(),
        ];
        for edit in edits {
            let mut changed = names.clone();
            edit(&mut changed);
            assert!(!binding.accepts(&changed, &Map::new(), None));
        }
        assert!(!binding.accepts(&names, &Map::new(), Some(&json!({}))));
        assert!(!binding.accepts(
            &names,
            &json!({"new":"value"}).as_object().unwrap().clone(),
            None
        ));
    }
}
