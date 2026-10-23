use super::JsonError;
use hyper::{
    HeaderMap, Method, StatusCode, Uri,
    header::{HeaderName, HeaderValue},
};

pub(crate) fn is_route(uri: &Uri) -> bool {
    [
        "/storage/v1/b",
        "/upload/storage/v1/b",
        "/download/storage/v1/b",
    ]
    .iter()
    .any(|base| {
        uri.path() == *base
            || uri
                .path()
                .strip_prefix(base)
                .is_some_and(|s| s.starts_with('/'))
    })
}

pub(crate) fn origin(headers: &HeaderMap) -> Option<&HeaderValue> {
    let mut values = headers.get_all("origin").iter();
    let origin = values.next()?;
    let value = origin.to_str().ok()?;
    if values.next().is_some() || value.is_empty() || value.contains([',', '@', '?', '#']) {
        return None;
    }
    if value != "null" {
        let uri: Uri = value.parse().ok()?;
        let (scheme, authority) = (uri.scheme_str()?, uri.authority()?);
        if value != format!("{scheme}://{authority}") || authority.host().is_empty() {
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

pub(crate) fn requested_headers(headers: &HeaderMap) -> Result<Vec<HeaderName>, JsonError> {
    let mut names = Vec::new();
    for value in headers.get_all("access-control-request-headers") {
        let value = value
            .to_str()
            .map_err(|_| JsonError::invalid_argument("Invalid CORS request headers"))?;
        for name in value.split(',') {
            names.push(
                HeaderName::from_bytes(name.trim().as_bytes())
                    .map_err(|_| JsonError::invalid_argument("Invalid CORS request headers"))?,
            );
        }
    }
    Ok(names)
}

pub(crate) fn response_headers(
    headers: &HeaderMap,
    preflight: bool,
) -> Result<HeaderMap, JsonError> {
    let mut result = HeaderMap::new();
    result.insert("vary", HeaderValue::from_static("Origin"));
    let Some(origin) = origin(headers) else {
        return Ok(result);
    };
    result.insert("access-control-allow-origin", origin.clone());
    if preflight {
        result.insert(
            "vary",
            HeaderValue::from_static(
                "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
            ),
        );
        result.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("DELETE, GET, HEAD, PATCH, POST, PUT"),
        );
        let requested = requested_headers(headers)?;
        if !requested.is_empty() {
            let names = requested
                .iter()
                .map(HeaderName::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            result.insert("access-control-allow-headers", names.parse().unwrap());
        }
        result.insert("access-control-max-age", HeaderValue::from_static("3600"));
    } else {
        result.insert(
            "access-control-allow-credentials",
            HeaderValue::from_static("true"),
        );
        // Google documents common exposed headers without enumerating the provider's complete set.
        result.insert("access-control-expose-headers", HeaderValue::from_static("Cache-Control, Content-Disposition, Content-Encoding, Content-Language, Content-Length, Content-Range, Content-Type, Date, ETag, Expires, Last-Modified, Location, Range"));
    }
    Ok(result)
}

pub(crate) fn preflight_status(headers: &HeaderMap) -> StatusCode {
    if headers
        .get("access-control-request-method")
        .is_none_or(|v| v.as_bytes().trim_ascii().is_empty())
    {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    }
}

pub(crate) fn is_preflight(method: &Method, headers: &HeaderMap) -> bool {
    method == Method::OPTIONS && origin(headers).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origin_syntax_is_independent_of_listener_and_url_schemes() {
        for value in [
            "http://localhost:3000",
            "https://app.example",
            "ws://app.example:8080",
            "wss://app.example",
            "ftp://app.example",
            "chrome-extension://abcdefghijklmnop",
            "capacitor://localhost",
            "ionic://localhost",
            "app+local.v1://desktop",
            "null",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("origin", value.parse().unwrap());
            assert_eq!(origin(&headers).unwrap(), value);
        }
        for value in [
            "",
            "null, null",
            "https://app.example/path",
            "https://user@app.example",
            "https://app.example?query",
            "blob:https://app.example/id",
            "data:text/html,test",
            "file:///document.html",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("origin", value.parse().unwrap());
            assert!(origin(&headers).is_none(), "{value}");
        }
    }
}
