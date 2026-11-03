use super::http::Reply;
use serde_json::{Value, json};

impl Reply {
    /// A bare text page: the status, content type and the whole body are compared.
    pub(crate) fn plain_error(&self, status: u16, content_type: &str, body: &str) {
        self.expect(status);
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
        self.expect(status);
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
        self.expect(status);
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
        self.expect(status);
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
        self.expect(status);
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

pub(crate) enum ContentType {
    Check,
    Skip,
}
impl Reply {
    pub(crate) fn xml_error(&self, status: u16, code: &str, content_type: ContentType) {
        self.expect(status);
        if matches!(content_type, ContentType::Check) {
            assert_eq!(
                self.headers["content-type"],
                "application/xml; charset=UTF-8"
            );
        }
        assert!(
            String::from_utf8_lossy(&self.body).contains(&format!("<Code>{code}</Code>")),
            "{}",
            String::from_utf8_lossy(&self.body)
        );
    }
}
pub(crate) fn xml_error(reply: &Reply, status: u16, code: &str) {
    reply.xml_error(status, code, ContentType::Skip);
}
pub(crate) fn xml_error_with_content_type(reply: &Reply, status: u16, code: &str) {
    reply.xml_error(status, code, ContentType::Check);
}

impl Reply {
    pub(crate) fn range_error(&self, status: u16, reason: &str) {
        self.expect(status);
        // A missing media object and an unsatisfiable range are plain HTML-typed messages.
        if self.headers["content-type"] == "text/html; charset=UTF-8" {
            let body = String::from_utf8_lossy(&self.body);
            match (status, reason) {
                (404, "notFound") => assert!(body.starts_with("No such object: "), "{body}"),
                (412, "conditionNotMet") => assert_eq!(body, "Precondition Failed"),
                (416, "requestedRangeNotSatisfiable") => {
                    assert_eq!(body, "Request range not satisfiable")
                }
                _ => panic!("unexpected plain page {status} {reason}: {body}"),
            }
            return;
        }
        let value: Value = serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!("not a JSON error: {}", String::from_utf8_lossy(&self.body))
        });
        assert_eq!(value["error"]["code"], status);
        assert_eq!(value["error"]["errors"][0]["reason"], reason, "{value}");
    }
}
