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
    EmptyCondition,
    DocumentedCondition,
    AbortCondition,
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
            Self::EmptyCondition => format!(
                "at least one condition, for example {{\"{}\":30}}",
                name("age")
            ),
            Self::DocumentedCondition => format!(
                "a documented lifecycle condition, for example {}",
                name("age")
            ),
            Self::AbortCondition => format!(
                "{}, {} or {}",
                name("age"),
                name("matchesPrefix"),
                name("matchesSuffix")
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
            Self::AutoclassTerminal => format!(
                "Invalid autoclass.{}; use NEARLINE or ARCHIVE",
                name("terminalStorageClass")
            ),
            Self::AutoclassDisabled => format!(
                "autoclass.{} requires autoclass.enabled=true",
                name("terminalStorageClass")
            ),
            Self::AutoclassStorageClass => format!(
                "An Autoclass bucket's {} must be STANDARD; omit {} or use STANDARD",
                name("storageClass"),
                name("storageClass")
            ),
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
