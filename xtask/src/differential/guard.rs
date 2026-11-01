//! Safety checks on the request that is about to be sent, after variable
//! expansion and percent-decoding, so a case cannot reach what it must not
//! through a captured variable, an encoded path, a header or an XML body.
//!
//! Three things must never be sent to a live project: a locked retention (it
//! cannot be undone), anything that makes data public (`allUsers`, public
//! canned ACLs, public access prevention relaxed), and an HMAC key creation
//! (the response carries a secret).

use super::template::percent_decode;
use super::transport::{Request, Response, Transport};
use serde_json::Value;

type Result<T> = std::result::Result<T, String>;

/// Lowercase with separators removed, so `lock_retention-Policy` and
/// `lockRetentionPolicy` compare equal.
fn flat(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '_' | '-') && !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `unlocked` / `unlock` are the safe spellings; remove them before looking
/// for `lock`.
fn without_unlock(flat_text: &str) -> String {
    flat_text.replace("unlocked", "").replace("unlock", "")
}

fn mentions_lock_call(flat_text: &str) -> bool {
    flat_text.contains("lockretentionpolicy") || flat_text.contains("lockbucketretentionpolicy")
}

/// Used for path, query and header text: a request that names both `lock` and
/// `retention` is refused, as is a value that is exactly `locked`.
fn lock_in_metadata(text: &str) -> bool {
    let flat_text = without_unlock(&flat(text));
    mentions_lock_call(&flat_text)
        || (flat_text.contains("lock") && flat_text.contains("retention"))
}

/// A JSON document that locks retention. Only a literal false `isLocked` is
/// known to be unlocked, and a retention `mode` must be a plain string other
/// than Locked (gRPC JSON also accepts the enum number, LOCKED is 2).
pub(crate) fn json_locks(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let unlocked = matches!(item, Value::Bool(false))
                    || item
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case("false"));
                if flat(key) == "islocked" && !unlocked {
                    return Some("isLocked is not literally false".into());
                }
                if key.eq_ignore_ascii_case("mode")
                    && !item
                        .as_str()
                        .is_some_and(|s| !s.eq_ignore_ascii_case("locked") && !s.contains('{'))
                {
                    return Some("retention mode is Locked or not a plain string".into());
                }
                if mentions_lock_call(&flat(key)) {
                    return Some("lockRetentionPolicy".into());
                }
                if let Some(reason) = json_locks(item) {
                    return Some(reason);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(json_locks),
        Value::String(text) if mentions_lock_call(&flat(text)) => {
            Some("lockRetentionPolicy".into())
        }
        _ => None,
    }
}

/// Every JSON document in a body: one value, or newline-delimited messages.
fn json_documents(text: &str) -> Vec<Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .map_while(std::result::Result::ok)
        .collect()
}

fn key_is_predefined_acl(key: &str) -> bool {
    matches!(
        flat(key).as_str(),
        "predefinedacl" | "predefineddefaultobjectacl"
    )
}

fn json_public(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if key_is_predefined_acl(key)
                    && item.as_str().is_some_and(|s| flat(s).contains("public"))
                {
                    return Some("a public predefined ACL".into());
                }
                if flat(key) == "publicaccessprevention" {
                    let allowed = item.as_str().is_some_and(|s| {
                        s.eq_ignore_ascii_case("enforced") || s.eq_ignore_ascii_case("bogus")
                    });
                    if !allowed {
                        return Some("public access prevention other than enforced".into());
                    }
                }
                if let Some(reason) = json_public(item) {
                    return Some(reason);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(json_public),
        _ => None,
    }
}

/// The path and the decoded query pairs of an HTTP target.
fn split_target(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let pairs = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                percent_decode(&key.replace('+', " ")),
                percent_decode(&value.replace('+', " ")),
            )
        })
        .collect();
    (percent_decode(path), pairs)
}

fn check_locked(
    request: &Request,
    path: &str,
    query: &[(String, String)],
    text: &str,
) -> Result<()> {
    let metadata = std::iter::once(request.method.as_str())
        .chain(std::iter::once(path))
        .chain(query.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]))
        .chain(
            request
                .headers
                .iter()
                .flat_map(|(k, v)| [k.as_str(), v.as_str()]),
        );
    for part in metadata {
        if lock_in_metadata(part) || part.trim().eq_ignore_ascii_case("locked") {
            return Err("locked retention is never requested".into());
        }
    }
    let body_flat = flat(text);
    if mentions_lock_call(&body_flat) || body_flat.contains(">locked<") {
        return Err("locked retention is never requested (body)".into());
    }
    if let Some(reason) = json_documents(text).iter().find_map(json_locks) {
        return Err(format!("locked retention is never requested ({reason})"));
    }
    Ok(())
}

fn check_public(
    request: &Request,
    path: &str,
    query: &[(String, String)],
    text: &str,
) -> Result<()> {
    let everything = flat(&format!(
        "{path} {} {} {text}",
        query
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&"),
        request
            .headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    if everything.contains("allusers") || everything.contains("allauthenticatedusers") {
        return Err("allUsers and allAuthenticatedUsers are never granted".into());
    }
    let public_value = |value: &str| flat(value).starts_with("public");
    if query
        .iter()
        .any(|(k, v)| key_is_predefined_acl(k) && public_value(v))
        || request
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-goog-acl") && public_value(v))
    {
        return Err("a public canned ACL is never requested".into());
    }
    if let Some(reason) = json_documents(text).iter().find_map(json_public) {
        return Err(format!("never requested: {reason}"));
    }
    Ok(())
}

/// Creating an HMAC key returns its secret once; the harness must not hold it.
fn check_hmac(request: &Request, path: &str, query: &[(String, String)]) -> Result<()> {
    let creates = request.method == "POST" && path.trim_end_matches('/').ends_with("/hmacKeys");
    let names_account = query
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("serviceAccountEmail") && !v.is_empty());
    if creates && names_account {
        return Err("creating an HMAC key would expose its secret".into());
    }
    if request.method.ends_with("/CreateHmacKey") {
        return Err("creating an HMAC key would expose its secret".into());
    }
    Ok(())
}

/// Refuses a request that must never reach a live project.
pub(crate) fn check_request(request: &Request) -> Result<()> {
    let (path, query) = split_target(&request.target);
    let text = request
        .body
        .as_deref()
        .map(|body| String::from_utf8_lossy(body).into_owned())
        .unwrap_or_default();
    check_locked(request, &path, &query, &text)?;
    check_public(request, &path, &query, &text)?;
    check_hmac(request, &path, &query)
}

/// Wraps a transport so every request is checked first. The check sits at the
/// last point before the bytes leave, after expansion and encoding.
pub(crate) struct Guarded<'a> {
    pub(crate) inner: &'a dyn Transport,
}

impl Transport for Guarded<'_> {
    fn send(&self, request: &Request) -> Result<Response> {
        check_request(request).map_err(|e| format!("request refused before sending: {e}"))?;
        self.inner.send(request)
    }

    fn requests(&self) -> u64 {
        self.inner.requests()
    }

    fn take_retries(&self) -> u32 {
        self.inner.take_retries()
    }
}

/// Decodes a `body_base64` value for the test below.
#[cfg(test)]
fn bytes_of(text: &str) -> Vec<u8> {
    crate::differential::base64::decode(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(method: &str, target: &str, headers: &[(&str, &str)], body: Option<&str>) -> Request {
        Request {
            method: method.into(),
            target: target.into(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            body: body.map(|b| b.as_bytes().to_vec()),
        }
    }

    fn refused(request: &Request) -> bool {
        check_request(request).is_err()
    }

    #[test]
    fn refuses_locks_wherever_they_hide() {
        assert!(refused(&http(
            "POST",
            "/b/x/lockRetentionPolicy?ifMetagenerationMatch=1",
            &[],
            None
        )));
        assert!(refused(&http(
            "POST",
            "/b/x/lock%52etentionPolicy",
            &[],
            None
        )));
        assert!(refused(&http(
            "POST",
            "/b/x",
            &[("X-Op", "lock-retention")],
            None
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some("{\"retentionPolicy\":{\"isLocked\":true}}")
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some("{\"retention\":{\"mode\":\"Locked\"}}")
        )));
        assert!(refused(&http(
            "PUT",
            "/b/x?retention",
            &[],
            Some("<RetentionConfiguration><Mode> Locked </Mode></RetentionConfiguration>")
        )));
        let grpc = Request {
            method: "google.storage.v2.Storage/LockBucketRetentionPolicy".into(),
            target: String::new(),
            headers: Vec::new(),
            body: Some(b"{}\n".to_vec()),
        };
        assert!(refused(&grpc));
        assert!(refused(&http("GET", "/b/x?mode=locked", &[], None)));
    }

    #[test]
    fn allows_the_unlocked_spellings() {
        assert!(!refused(&http(
            "PATCH",
            "/b/x/o/y?overrideUnlockedRetention=true",
            &[],
            Some("{\"retention\":null}")
        )));
        assert!(!refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some(
                "{\"retentionPolicy\":{\"isLocked\":false},\"retention\":{\"mode\":\"Unlocked\"}}"
            )
        )));
        assert!(!refused(&http(
            "GET",
            "/b/x/o/obj-retention-unlocked-1",
            &[],
            None
        )));
    }

    #[test]
    fn refuses_public_access_in_every_position() {
        assert!(refused(&http(
            "PUT",
            "/b/x/iam",
            &[],
            Some("{\"bindings\":[{\"members\":[\"allUsers\"]}]}")
        )));
        assert!(refused(&http(
            "PUT",
            "/b/x/iam",
            &[],
            Some("{\"members\":[\"all_authenticated_users\"]}")
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x?predefinedAcl=publicRead",
            &[],
            None
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x?predefined%41cl=public-read",
            &[],
            None
        )));
        assert!(refused(&http(
            "PUT",
            "/x/y",
            &[("x-goog-acl", "public-read")],
            None
        )));
        assert!(refused(&http(
            "PUT",
            "/x/y?acl",
            &[],
            Some(
                "<AccessControlList><Entry><Scope type=\"AllUsers\"/></Entry></AccessControlList>"
            )
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some("{\"predefined_acl\":\"PUBLIC_READ\"}")
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some("{\"iamConfiguration\":{\"publicAccessPrevention\":\"inherited\"}}")
        )));
        assert!(refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some("{\"iamConfiguration\":{\"publicAccessPrevention\":null}}")
        )));
        assert!(!refused(&http(
            "PATCH",
            "/b/x",
            &[],
            Some("{\"iamConfiguration\":{\"publicAccessPrevention\":\"enforced\"}}")
        )));
        assert!(!refused(&http(
            "PATCH",
            "/b/x?predefinedAcl=private",
            &[],
            None
        )));
    }

    #[test]
    fn refuses_hmac_key_creation_for_an_account() {
        assert!(refused(&http(
            "POST",
            "/storage/v1/projects/p/hmacKeys?serviceAccountEmail=a%40b.c",
            &[],
            None
        )));
        assert!(!refused(&http(
            "POST",
            "/storage/v1/projects/p/hmacKeys",
            &[],
            None
        )));
        assert!(!refused(&http(
            "GET",
            "/storage/v1/projects/p/hmacKeys/GOOG1",
            &[],
            None
        )));
    }

    #[test]
    fn binary_bodies_are_inspected_as_text() {
        let request = Request {
            body: Some(bytes_of("eyJpc0xvY2tlZCI6IHRydWV9")),
            ..http("PATCH", "/b/x", &[], None)
        };
        assert!(refused(&request));
    }
}
