use super::http::Reply;
use serde_json::{Value, json};

impl Reply {
    /// A bare text page: the status, content type and the whole body are compared.
    pub(crate) fn plain_error(&self, status: u16, content_type: &str, body: &str) {
        assert_eq!(self.status, status);
        assert_eq!(self.headers["content-type"], content_type);
        assert_eq!(String::from_utf8_lossy(&self.body), body);
    }

    /// A delete or overwrite refused by a hold: a 403 `forbidden` whose message names the hold.
    pub(crate) fn hold_error(&self) {
        self.json_reason(403, "forbidden");
        let message = String::from_utf8_lossy(&self.body).into_owned();
        assert!(
            message.contains("under active Temporary hold")
                || message.contains("under active Event-Based hold"),
            "{message}"
        );
    }

    pub(crate) fn json_error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        // The upload front end labelled some JSON envelopes text/html (recorded).
        let content_type = self.headers["content-type"].as_str();
        assert!(
            matches!(
                content_type,
                "application/json; charset=UTF-8" | "text/html; charset=UTF-8"
            ),
            "{content_type}"
        );
        let body: Value = serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!("not a JSON error: {}", String::from_utf8_lossy(&self.body))
        });
        let error = &body["error"];
        assert_eq!(error["code"], status);
        assert_eq!(error["errors"][0]["domain"], "global");
        assert_eq!(error["errors"][0]["reason"], reason, "{error}");
        assert_eq!(error["errors"][0]["message"], error["message"]);
    }
}

impl Reply {
    pub(crate) fn metadata_error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        // A missing media object is an HTML document naming the object, not a JSON error.
        if self.headers["content-type"] == "text/html; charset=UTF-8" {
            assert_eq!((status, reason), (404, "notFound"));
            assert!(self.body.starts_with(b"No such object: "));
            return;
        }
        let value: Value = serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!("not a JSON error: {}", String::from_utf8_lossy(&self.body))
        });
        assert_eq!(value["error"]["errors"][0]["reason"], reason, "{value}");
    }
}
impl Reply {
    /// A HEAD reply is checked for stray body bytes until EOF, so callers send HEAD
    /// requests through [`close_after_response`] to have the persistent connection closed.

    pub(crate) fn success(&self) -> &Self {
        assert!(
            matches!(self.status, 200 | 204),
            "{}: {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
        self
    }

    pub(crate) fn auth_error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(
            self.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        assert_eq!(self.json()["error"]["errors"][0]["reason"], reason);
        assert!(!self.headers.contains_key("www-authenticate"));
    }

    pub(crate) fn login_required(&self) {
        self.auth_error(401, "required");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"required","message":"Login Required","locationType":"header","location":"Authorization"}],"code":401,"message":"Login Required"}})
        );
    }

    pub(crate) fn invalid_credentials(&self) {
        self.auth_error(401, "authError");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"authError","message":"Invalid Credentials","locationType":"header","location":"Authorization"}],"code":401,"message":"Invalid Credentials"}})
        );
    }

    pub(crate) fn forbidden(&self) {
        self.auth_error(403, "forbidden");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"forbidden","message":"Forbidden"}],"code":403,"message":"Forbidden"}})
        );
    }
}
impl super::http::TextReply {
    /// A delete or overwrite refused by a hold: a 403 `forbidden` whose message names the hold.
    pub(crate) fn hold_error(&self) {
        self.json_envelope(403, "forbidden");
        assert!(
            self.body.contains("under active Temporary hold")
                || self.body.contains("under active Event-Based hold"),
            "{}",
            self.body
        );
    }

    pub(crate) fn json_envelope(&self, status: u16, reason: &str) {
        assert_eq!(self.status, status, "{}", self.body);
        assert!(self.headers.contains("content-type: application/json"));
        let value = self.json();
        assert_eq!(value["error"]["code"], status);
        assert_eq!(value["error"]["errors"][0]["reason"], reason);
        assert_eq!(value["error"]["errors"][0]["domain"], "global");
        assert_eq!(
            value["error"]["message"],
            value["error"]["errors"][0]["message"]
        );
    }
}
