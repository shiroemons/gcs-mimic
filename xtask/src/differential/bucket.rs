//! Creation and cleanup of the disposable buckets the harness owns.

use super::normalize::project_numbers;
use super::runner::Side;
use super::template::percent_encode;
use super::transport::{Request, Response};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

type Result<T> = std::result::Result<T, String>;

fn send_retrying(side: &Side<'_>, request: &Request) -> Result<Response> {
    let mut attempt = 0;
    loop {
        let response = side.transport.send(request)?;
        if !matches!(response.status, 429 | 500 | 502 | 503 | 504) || attempt == 3 {
            return Ok(response);
        }
        attempt += 1;
        sleep(Duration::from_secs(3 * attempt));
    }
}

fn request(method: &str, target: String, body: Option<Value>) -> Request {
    Request {
        method: method.into(),
        target,
        headers: body
            .iter()
            .map(|_| ("Content-Type".to_owned(), "application/json".to_owned()))
            .collect(),
        body: body.map(|b| b.to_string().into_bytes()),
    }
}

fn describe(response: &Response) -> String {
    format!(
        "HTTP {}: {}",
        response.status,
        String::from_utf8_lossy(&response.body).trim()
    )
}

/// Creates a us-central1 STANDARD bucket with soft delete off. Returns the
/// project numbers found in the response so they can be redacted later.
pub(crate) fn create(side: &Side<'_>, name: &str) -> Result<Vec<String>> {
    let body = json!({
        "name": name,
        "location": "us-central1",
        "storageClass": "STANDARD",
        "softDeletePolicy": {"retentionDurationSeconds": "0"},
    });
    let target = format!("/storage/v1/b?project={}", percent_encode(side.project));
    let response = send_retrying(side, &request("POST", target, Some(body)))?;
    if response.status != 200 {
        return Err(describe(&response));
    }
    let mut numbers = Vec::new();
    if let Ok(json) = serde_json::from_slice(&response.body) {
        project_numbers(&json, &mut numbers);
    }
    Ok(numbers)
}

/// Empties and deletes a bucket, then confirms it answers 404. A bucket that is
/// already gone is not an error.
pub(crate) fn cleanup(side: &Side<'_>, name: &str) -> Result<String> {
    let mut page: Option<String> = None;
    let mut deleted = 0;
    loop {
        let mut target = format!(
            "/storage/v1/b/{name}/o?versions=true&fields=items(name,generation),nextPageToken"
        );
        if let Some(token) = &page {
            target.push_str(&format!("&pageToken={}", percent_encode(token)));
        }
        let response = send_retrying(side, &request("GET", target, None))?;
        if response.status == 404 {
            return Ok("already absent (404)".into());
        }
        if response.status != 200 {
            return Err(format!("list objects: {}", describe(&response)));
        }
        let listing: Value = serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
        for item in listing["items"].as_array().into_iter().flatten() {
            let object = item["name"].as_str().ok_or("object without name")?;
            let generation = item["generation"]
                .as_str()
                .ok_or("object without generation")?;
            let target = format!(
                "/storage/v1/b/{name}/o/{}?generation={generation}",
                percent_encode(object)
            );
            let response = send_retrying(side, &request("DELETE", target, None))?;
            if !matches!(response.status, 204 | 404) {
                return Err(format!("delete object: {}", describe(&response)));
            }
            deleted += 1;
        }
        match listing["nextPageToken"].as_str() {
            Some(token) => page = Some(token.to_owned()),
            None => break,
        }
    }
    let response = send_retrying(
        side,
        &request("DELETE", format!("/storage/v1/b/{name}"), None),
    )?;
    if !matches!(response.status, 204 | 404) {
        return Err(format!("delete bucket: {}", describe(&response)));
    }
    let check = send_retrying(side, &request("GET", format!("/storage/v1/b/{name}"), None))?;
    if check.status != 404 {
        return Err(format!("bucket still answers: {}", describe(&check)));
    }
    Ok(format!(
        "emptied ({deleted} object versions), deleted, GET confirmed 404"
    ))
}
