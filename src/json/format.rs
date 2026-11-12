//! Serialization of JSON API response bodies.
//!
//! Cloud Storage pretty-prints every JSON API body with two-space indentation unless the
//! request carries the standard Google API parameter `prettyPrint=false`. Response bodies are
//! rendered deep inside the protocol handlers, so the routing layer selects the format once per
//! request with [`with_format`] instead of threading it through every handler signature.

use crate::config::CompatProfile;
use serde_json::Value;
use std::future::Future;

/// How a JSON API response body is serialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseFormat {
    /// Two-space indentation, the Cloud Storage default.
    Pretty,
    /// No insignificant whitespace, for `prettyPrint=false` and the fake-gcs-server profile.
    Compact,
}

tokio::task_local! {
    static FORMAT: ResponseFormat;
}

impl ResponseFormat {
    /// The format a request selects. Each method validates the `prettyPrint` value itself, so an
    /// invalid value fails with 400 and its error body uses the default format.
    pub(crate) fn for_request(query: Option<&str>, profile: CompatProfile) -> Self {
        // fake-gcs-server writes compact JSON, which the fake-gcs-server profile reproduces.
        if profile == CompatProfile::FakeGcsServer {
            return Self::Compact;
        }
        // The spellings match the shared boolean query parser.
        let compact = matches!(
            super::query::query_param(query.unwrap_or_default(), "prettyPrint").as_deref(),
            Some("false" | "False")
        );
        if compact { Self::Compact } else { Self::Pretty }
    }
}

/// Runs a request handler with the response format its bodies use.
pub(crate) async fn with_format<F: Future>(format: ResponseFormat, handler: F) -> F::Output {
    FORMAT.scope(format, handler).await
}

/// Serializes a response body in the current request's format; outside a request, pretty.
pub(crate) fn render_body(value: &Value) -> String {
    match FORMAT.try_with(|format| *format) {
        Ok(ResponseFormat::Compact) => value.to_string(),
        Ok(ResponseFormat::Pretty) | Err(_) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prettyprint_false_selects_compact_output_and_everything_else_pretty() {
        let native = CompatProfile::Native;
        for (query, expected) in [
            (None, ResponseFormat::Pretty),
            (Some("prettyPrint=true"), ResponseFormat::Pretty),
            (Some("alt=json&prettyPrint=false"), ResponseFormat::Compact),
            (Some("prettyPrint=False"), ResponseFormat::Compact),
            (Some("%70rettyPrint=%66alse"), ResponseFormat::Compact),
            (Some("prettyPrint=%46alse"), ResponseFormat::Compact),
            (Some("xprettyPrint=false"), ResponseFormat::Pretty),
        ] {
            assert_eq!(
                ResponseFormat::for_request(query, native),
                expected,
                "{query:?}"
            );
        }
        assert_eq!(
            ResponseFormat::for_request(None, CompatProfile::FakeGcsServer),
            ResponseFormat::Compact
        );
    }

    #[tokio::test]
    async fn rendering_follows_the_scoped_format_and_keeps_field_order() {
        let value = json!({"kind": "storage#object", "id": "b/o/1", "size": "0"});
        assert_eq!(
            render_body(&value),
            "{\n  \"kind\": \"storage#object\",\n  \"id\": \"b/o/1\",\n  \"size\": \"0\"\n}"
        );
        let compact = with_format(ResponseFormat::Compact, async { render_body(&value) }).await;
        assert_eq!(
            compact,
            r#"{"kind":"storage#object","id":"b/o/1","size":"0"}"#
        );
    }
}
