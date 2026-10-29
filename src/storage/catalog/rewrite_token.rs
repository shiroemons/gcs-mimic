use serde_json::{Map, Value};

/// An unguessable database key: its authoritative request binding stays in the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewriteToken(String);

impl RewriteToken {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(crate::hashing::encode_lower(&bytes)))
    }

    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Includes the selected immutable generation, even when sourceGeneration was omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewriteBinding {
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
    pub fn accepts(
        &self,
        path: [&str; 4],
        supplied_parameters: &Map<String, Value>,
        supplied_metadata: Option<&Value>,
    ) -> bool {
        path == [
            self.source_bucket.as_str(),
            self.source_object.as_str(),
            self.destination_bucket.as_str(),
            self.destination_object.as_str(),
        ] && supplied_parameters
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
        assert_eq!(token.as_str().len(), 64);
        assert_eq!(RewriteToken::parse(token.as_str()).as_ref(), Some(&token));
        assert_ne!(token, RewriteToken::generate().unwrap());
        for index in 0..64 {
            let mut tampered = token.as_str().as_bytes().to_vec();
            tampered[index] = if tampered[index] == b'0' { b'1' } else { b'0' };
            assert_ne!(
                RewriteToken::parse(std::str::from_utf8(&tampered).unwrap()).as_ref(),
                Some(&token)
            );
        }
        for invalid in [
            "",
            "../object",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64),
        ] {
            assert!(RewriteToken::parse(invalid).is_none());
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
        let path = ["source", "a/b%日本", "destination", "c/d"];
        assert!(binding.accepts(path, &parameters, Some(&body)));
        assert!(binding.accepts(path, &Map::new(), None));
        for key in parameters.keys() {
            let mut changed = parameters.clone();
            changed.insert(key.clone(), json!("different"));
            assert!(!binding.accepts(path, &changed, None), "{key}");
        }
        for index in 0..4 {
            let mut changed = path;
            changed[index] = "different";
            assert!(!binding.accepts(changed, &Map::new(), None));
        }
        assert!(!binding.accepts(path, &Map::new(), Some(&json!({}))));
        assert!(!binding.accepts(
            path,
            &json!({"new":"value"}).as_object().unwrap().clone(),
            None
        ));
    }
}
