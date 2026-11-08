use super::http::Reply;
use serde_json::{Value, json};

impl Reply {
    /// A bare text page: the status, content type and the whole body are compared.
    #[track_caller]
    pub(crate) fn plain_error(&self, status: u16, content_type: &str, body: &str) {
        self.expect(status);
        assert_eq!(self.headers["content-type"], content_type);
        assert_eq!(String::from_utf8_lossy(&self.body), body);
    }

    /// A delete or overwrite refused by a hold: a 403 `forbidden` whose message names the hold.
    #[track_caller]
    pub(crate) fn hold_error_reason(&self) {
        self.json_reason(403, "forbidden");
        let message = String::from_utf8_lossy(&self.body).into_owned();
        assert!(
            message.contains("under active Temporary hold")
                || message.contains("under active Event-Based hold"),
            "{message}"
        );
    }

    #[track_caller]
    /// Checks upload MIME types, status, code, domain, reason and matching messages.
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
    #[track_caller]
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
    #[track_caller]
    pub(crate) fn success(&self) -> &Self {
        assert!(
            matches!(self.status, 200 | 204),
            "{}: {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
        self
    }

    #[track_caller]
    pub(crate) fn auth_error(&self, status: u16, reason: &str) {
        self.expect(status);
        assert_eq!(
            self.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        assert_eq!(self.json()["error"]["errors"][0]["reason"], reason);
        assert!(!self.headers.contains_key("www-authenticate"));
    }

    #[track_caller]
    pub(crate) fn login_required(&self) {
        self.auth_error(401, "required");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"required","message":"Login Required","locationType":"header","location":"Authorization"}],"code":401,"message":"Login Required"}})
        );
    }

    #[track_caller]
    pub(crate) fn invalid_credentials(&self) {
        self.auth_error(401, "authError");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"authError","message":"Invalid Credentials","locationType":"header","location":"Authorization"}],"code":401,"message":"Invalid Credentials"}})
        );
    }

    /// A registered caller refused a permission: the whole envelope, with a message in Cloud
    /// Storage's denial form. [`Self::denied`] also names the principal and the permission.
    #[track_caller]
    pub(crate) fn forbidden(&self) {
        self.auth_error(403, "forbidden");
        let message = self.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let (principal, _, _) = parse_denial(&message);
        assert_ne!(principal, ANONYMOUS, "{message}");
        assert_eq!(self.json(), forbidden_body(&message));
    }

    /// `principal` refused `permission` on a `resource` of `project`, `bucket` or `object`.
    #[track_caller]
    pub(crate) fn denied(&self, principal: &str, permission: &str, resource: &str) {
        self.auth_error(403, "forbidden");
        let message = denial_message(principal, permission, resource);
        assert_eq!(self.json(), forbidden_body(&message));
    }

    /// The anonymous caller refused `permission`: 401 `required` at the `Authorization` header,
    /// whose message names the anonymous caller and the permission.
    #[track_caller]
    pub(crate) fn anonymous_denied(&self, permission: &str, resource: &str) {
        self.auth_error(401, "required");
        let message = denial_message(ANONYMOUS, permission, resource);
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"required","message":message,"locationType":"header","location":"Authorization"}],"code":401,"message":message}})
        );
    }
}

/// How Cloud Storage's denial messages name a caller without credentials.
pub(crate) const ANONYMOUS: &str = "Anonymous caller";

/// The denial message for `principal` refused `permission` on a `resource` of `project`,
/// `bucket` or `object`.
#[track_caller]
pub(crate) fn denial_message(principal: &str, permission: &str, resource: &str) -> String {
    let noun = match resource {
        "project" => "Google Cloud project",
        "bucket" => "Google Cloud Storage bucket",
        "object" => "Google Cloud Storage object",
        other => panic!("unknown resource kind {other}"),
    };
    format!(
        "{principal} does not have {permission} access to the {noun}. Permission '{permission}' denied on resource (or it may not exist)."
    )
}

/// The principal, permission and resource kind of a message in the denial form; any other
/// message fails.
#[track_caller]
pub(crate) fn parse_denial(message: &str) -> (&str, &str, &'static str) {
    let parsed = message
        .split_once(" does not have ")
        .and_then(|(principal, rest)| Some((principal, rest.split_once(' ')?.0)))
        .and_then(|(principal, permission)| {
            ["project", "bucket", "object"]
                .into_iter()
                .find(|resource| denial_message(principal, permission, resource) == message)
                .map(|resource| (principal, permission, resource))
        });
    let Some(parsed @ (principal, permission, _)) = parsed else {
        panic!("not a denial message: {message}");
    };
    assert!(!principal.is_empty(), "{message}");
    assert!(permission.starts_with("storage."), "{message}");
    parsed
}

fn forbidden_body(message: &str) -> Value {
    json!({"error":{"errors":[{"domain":"global","reason":"forbidden","message":message}],"code":403,"message":message}})
}
impl super::http::TextReply {
    /// A delete or overwrite refused by a hold: a 403 `forbidden` whose message names the hold.
    #[track_caller]
    pub(crate) fn hold_error_envelope(&self) {
        self.json_envelope(403, "forbidden");
        assert!(
            self.body.contains("under active Temporary hold")
                || self.body.contains("under active Event-Based hold"),
            "{}",
            self.body
        );
    }

    #[track_caller]
    /// Checks the JSON MIME prefix, status, code, domain, reason and matching messages.
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
    #[track_caller]
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
#[track_caller]
pub(crate) fn xml_error(reply: &Reply, status: u16, code: &str) {
    reply.xml_error(status, code, ContentType::Skip);
}
/// An XML error whose `Message` is `message`; the message holds no markup to escape.
#[track_caller]
pub(crate) fn xml_error_message(reply: &Reply, status: u16, code: &str, message: &str) {
    reply.xml_error(status, code, ContentType::Check);
    let body = String::from_utf8_lossy(&reply.body);
    assert!(
        body.contains(&format!("<Message>{message}</Message>")),
        "{body}"
    );
}
#[track_caller]
pub(crate) fn xml_error_with_content_type(reply: &Reply, status: u16, code: &str) {
    reply.xml_error(status, code, ContentType::Check);
}

impl Reply {
    #[track_caller]
    pub(crate) fn html_or_json_error(&self, status: u16, reason: &str) {
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
