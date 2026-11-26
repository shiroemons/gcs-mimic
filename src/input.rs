//! Rejections of request input shared by the protocol adapters.

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct InputError {
    pub(crate) kind: InputKind,
    pub(crate) message: String,
    /// A parameter with a wire location. Bucket resource members are named in the message.
    pub(crate) field: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputKind {
    Invalid,
    Required,
    Unsupported,
}

impl InputError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: InputKind::Invalid,
            message: message.into(),
            field: None,
        }
    }

    pub(crate) fn missing(field: &str) -> Self {
        Self {
            kind: InputKind::Required,
            message: format!("Missing required {field}"),
            field: None,
        }
    }

    pub(crate) fn unsupported_bucket(field: &str) -> Self {
        Self {
            kind: InputKind::Unsupported,
            message: unsupported_bucket_setting(field),
            field: None,
        }
    }
}

/// A recognized bucket setting the emulator cannot apply, with the request that avoids it.
pub(crate) fn unsupported_bucket_setting(name: &str) -> String {
    format!(
        "gcs-mimic does not support bucket API setting {name}; omit this setting (for example, create a bucket with only name)."
    )
}
