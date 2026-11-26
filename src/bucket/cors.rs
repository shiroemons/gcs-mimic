//! How a browser's cross-origin request reads before bucket CORS rules are matched against it:
//! its `Origin` and whether it is a preflight. The JSON and XML APIs answer CORS differently
//! (CORS is not authorization), but they read the request alike.

use hyper::{HeaderMap, Uri, header::HeaderValue};

/// The request's single `Origin`, when it is `null` or a scheme and authority with nothing
/// else; any other value, or more than one, names no origin.
pub(crate) fn origin(headers: &HeaderMap) -> Option<&HeaderValue> {
    let origin = crate::http_header::sole(headers, "origin").value().ok()??;
    let value = origin.to_str().ok()?;
    if value.is_empty() || value.contains([',', '@', '?', '#']) {
        return None;
    }
    if value != "null" {
        let uri: Uri = value.parse().ok()?;
        let (scheme, authority) = (uri.scheme_str()?, uri.authority()?);
        if value
            .strip_prefix(scheme)
            .and_then(|v| v.strip_prefix("://"))
            != Some(authority.as_str())
            || authority.host().is_empty()
        {
            return None;
        }
        if authority
            .as_str()
            .rsplit_once(':')
            .is_some_and(|(_, port)| !port.contains(']') && port.parse::<u16>().is_err())
        {
            return None;
        }
    }
    Some(origin)
}

/// Whether a CORS request is a browser preflight or the actual request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CorsRequest {
    Preflight,
    Actual,
}
