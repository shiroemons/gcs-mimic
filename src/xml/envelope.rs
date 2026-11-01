//! Response headers Cloud Storage's XML API adds by request method.
//!
//! The recorded XML responses differ by method rather than by handler: reads and DELETE carry a
//! private `Cache-Control` with an `Expires` of the response time, PUT carries neither (it is
//! not cacheable), and PUT and DELETE vary by `Origin` even for a bucket without CORS rules. A
//! successful PUT with no body is `text/html`. Applying these once after routing keeps every
//! handler and error path consistent.

use super::PRIVATE_CACHE_CONTROL;
use crate::json::http_date;
use hyper::{
    Method, Response, StatusCode,
    header::{self, HeaderValue},
};
use std::time::SystemTime;

/// The `Content-Type` of an empty successful PUT response.
const EMPTY_PUT_CONTENT_TYPE: &str = "text/html; charset=UTF-8";

/// `Expires` is the response time: the response is private and must be revalidated at once.
pub(crate) fn expires(now: SystemTime) -> Option<HeaderValue> {
    http_date(now)
        .ok()
        .and_then(|date| HeaderValue::from_str(&date).ok())
}

pub(crate) fn apply<B>(method: &Method, now: SystemTime, response: &mut Response<B>) {
    let private = response
        .headers()
        .get(header::CACHE_CONTROL)
        .is_some_and(|value| value == PRIVATE_CACHE_CONTROL);
    let headers = response.headers_mut();
    if (method == Method::PUT || method == Method::DELETE) && !headers.contains_key(header::VARY) {
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    if method == Method::DELETE && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(PRIVATE_CACHE_CONTROL),
        );
    }
    if method == Method::PUT && private {
        headers.remove(header::CACHE_CONTROL);
    } else if (private || method == Method::DELETE)
        && !headers.contains_key(header::EXPIRES)
        && let Some(value) = expires(now)
    {
        headers.insert(header::EXPIRES, value);
    }
    if method == Method::PUT
        && response.status() == StatusCode::OK
        && !response.headers().contains_key(header::CONTENT_TYPE)
    {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(EMPTY_PUT_CONTENT_TYPE),
        );
    }
}
