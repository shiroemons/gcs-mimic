//! Semantic metadata failures, rendered with each adapter's field vocabulary.

#[derive(Debug, PartialEq, Eq)]
pub enum MetadataError {
    FixedHeader(String),
    EmptyLanguage,
    LongLanguage,
    CustomTime,
    ObjectStorageClass,
    StorageClassRequiresLocation,
    InvalidUpdateStorageClass(String),
    UnsupportedObjectStorageClass(String),
    Lifecycle {
        field: String,
        example: LifecycleExample,
    },
    LifecycleClass {
        field: String,
        value: String,
    },
    /// A lifecycle action type outside Delete, SetStorageClass and AbortIncompleteMultipartUpload.
    LifecycleActionType(String),
    /// A rule whose condition object names no condition.
    LifecycleEmptyCondition,
    /// An AbortIncompleteMultipartUpload rule with a condition other than age, prefix or suffix.
    LifecycleAbortCondition,
    AutoclassTerminal,
    AutoclassDisabled,
    AutoclassStorageClass,
    AutoclassLifecycle,
    UniformAccessLocked,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleExample {
    Literal(String),
    AgeCondition,
    DocumentedCondition,
    StorageClassAction,
}

impl From<&str> for LifecycleExample {
    fn from(value: &str) -> Self {
        Self::Literal(value.into())
    }
}

impl LifecycleExample {
    fn message(&self, name: fn(&str) -> &str) -> String {
        match self {
            Self::Literal(value) => value.clone(),
            Self::AgeCondition => format!("{{\"{}\":30}}", name("age")),
            Self::DocumentedCondition => format!(
                "a documented lifecycle condition, for example {}",
                name("age")
            ),
            Self::StorageClassAction => {
                format!("{} only with SetStorageClass", name("storageClass"))
            }
        }
    }
}

impl MetadataError {
    pub(crate) fn unsupported(&self) -> bool {
        matches!(
            self,
            Self::StorageClassRequiresLocation
                | Self::UnsupportedObjectStorageClass(_)
                | Self::LifecycleClass { .. }
        )
    }

    pub(crate) fn message(&self, name: fn(&str) -> &str) -> String {
        let path = |path: &str| path.split('.').map(name).collect::<Vec<_>>().join(".");
        match self {
            Self::FixedHeader(field) => format!("Invalid {} header value", name(field)),
            Self::EmptyLanguage => format!(
                "{} must not be empty; use null to clear it",
                name("contentLanguage")
            ),
            Self::LongLanguage => format!("{} exceeds 100 characters", name("contentLanguage")),
            Self::CustomTime => format!(
                "Parse Error: Invalid value for type.googleapis.com/google.protobuf.Timestamp field: 'Field '{}', Illegal timestamp format; timestamps must end with 'Z' or have a valid timezone offset.'.",
                name("customTime")
            ),
            Self::ObjectStorageClass => format!(
                "Invalid object {}; use STANDARD, NEARLINE, COLDLINE or ARCHIVE",
                name("storageClass")
            ),
            Self::StorageClassRequiresLocation => {
                format!("{} requires a region or zone", name("storageClass"))
            }
            Self::InvalidUpdateStorageClass(value) => format!("Invalid storage class \"{value}\""),
            Self::UnsupportedObjectStorageClass(value) => format!(
                "{}={value}: {} requires a region or zone",
                name("storageClass"),
                name("storageClass")
            ),
            Self::Lifecycle { field, example } => format!(
                "Invalid lifecycle {}; use {}",
                path(field),
                example.message(name)
            ),
            Self::LifecycleClass { field, value } => format!(
                "lifecycle.{}={value}: {} requires a region or zone",
                path(field),
                name("storageClass")
            ),
            // The next three are Cloud Storage's own texts (recorded 2026-11); the abort text
            // names the protobuf conditions on every protocol, as the provider's does.
            Self::LifecycleActionType(value) => {
                format!("Invalid value for: {value} is not a valid value")
            }
            Self::LifecycleEmptyCondition => "Lifecycle rules must have a condition.".into(),
            Self::LifecycleAbortCondition => "Lifecycle age_days, matches_prefix, and matches_suffix conditions are the only ones allowed for a ABORT_INCOMPLETE_MULTIPART_UPLOAD rule.".into(),
            // Cloud Storage's own texts for the two Autoclass refusals (recorded 2026-11-13).
            Self::AutoclassTerminal => "Invalid value for terminalStorageClass field in Autoclass config. Valid values are NEARLINE and ARCHIVE".into(),
            Self::AutoclassDisabled => format!(
                "autoclass.{} requires autoclass.enabled=true",
                name("terminalStorageClass")
            ),
            Self::AutoclassStorageClass => "Cannot set default storage class on bucket with Autoclass enabled to storage class other than STANDARD.".into(),
            Self::AutoclassLifecycle => format!(
                "Autoclass cannot be combined with lifecycle SetStorageClass actions or {} conditions; remove those rules",
                name("matchesStorageClass")
            ),
            Self::UniformAccessLocked => format!(
                "Uniform bucket-level access has been enabled for 90 consecutive days and cannot be disabled; keep {}.enabled=true",
                name("uniformBucketLevelAccess")
            ),
        }
    }
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message(|name| name))
    }
}

impl std::error::Error for MetadataError {}
