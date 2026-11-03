use crate::common;
use common::http::Client;
pub(crate) use common::http::Reply;
use serde_json::Value;
use std::time::Duration;

common::process::declare_server!();
impl Server {
    pub(crate) fn start(dir: &common::TestDir, strict: bool) -> Self {
        Self::start_with_catalog_wait(dir, strict, 30000)
    }
    pub(crate) fn start_with_catalog_wait(
        dir: &common::TestDir,
        strict: bool,
        milliseconds: u64,
    ) -> Self {
        Self::start_with_timeouts(dir, strict, milliseconds, 900000)
    }
    pub(crate) fn start_with_timeouts(
        dir: &common::TestDir,
        strict: bool,
        catalog_milliseconds: u64,
        request_milliseconds: u64,
    ) -> Self {
        Self::start_with_limits(
            dir,
            strict,
            catalog_milliseconds,
            request_milliseconds,
            gcs_mimic::config::Config::default().max_object_bytes,
        )
    }
    /// Permissive server with extra serve arguments.
    pub(crate) fn start_with_args(dir: &common::TestDir, extra: &[&str]) -> Self {
        let mut command = common::process::http_command(&dir.0);
        command
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .args(extra);
        Self::launch(&mut command)
    }
    pub(crate) fn start_with_limits(
        dir: &common::TestDir,
        strict: bool,
        catalog_milliseconds: u64,
        request_milliseconds: u64,
        max_object_bytes: u64,
    ) -> Self {
        let mut command = common::process::http_command(&dir.0);
        command
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .arg("--catalog-wait-timeout-ms")
            .arg(catalog_milliseconds.to_string())
            .arg("--object-request-timeout-ms")
            .arg(request_milliseconds.to_string())
            .arg("--max-object-bytes")
            .arg(max_object_bytes.to_string());
        if strict {
            command.args(common::process::strict_args(&dir.0, &serde_json::json!({"identities":[
                {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local":["roles/storage.objectViewer"]}},
                {"id":"creator@example.test","tokens":["creator"],"project_roles":{"local":["roles/storage.objectCreator"]}},
                {"id":"scoped@example.test","tokens":["scoped"]}
            ]})));
        }
        Self::launch(&mut command)
    }
    pub(crate) fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, headers, body)
    }
    pub(crate) fn update(&self, method: &str, name: &str, value: &Value) -> Reply {
        self.request(
            method,
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            "Authorization: Bearer admin\r\n",
            value.to_string().as_bytes(),
        )
    }
    pub(crate) fn resumable(&self, name: &str, metadata: &Value) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
            "Authorization: Bearer admin\r\n",
            metadata.to_string().as_bytes(),
        )
    }
    pub(crate) fn multipart(
        &self,
        boundary: &str,
        metadata: &str,
        media: &[u8],
        headers: &str,
    ) -> Reply {
        let mut body = format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: text/plain\r\n\r\n").into_bytes();
        body.extend_from_slice(media);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        Client {
            address: &self.address,
            host: "localhost",
            headers: "",
            timeout: Duration::from_secs(10),
        }
        .request_eof(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=multipart",
            &format!("{headers}Content-Type: multipart/related; boundary={boundary}\r\n"),
            &body,
        )
        .into_joined()
    }
    pub(crate) fn bucket(&self) {
        let reply = self.request(
            "POST",
            "/storage/v1/b?project=local",
            "Authorization: Bearer admin\r\n",
            br#"{"name":"test-bucket"}"#,
        );
        reply.json_ok();
    }
    pub(crate) fn put(&self, name: &str, bytes: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name={name}"),
            "Authorization: Bearer admin\r\n",
            bytes,
        )
    }
    pub(crate) fn get(&self, name: &str) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            "Authorization: Bearer admin\r\n",
            b"",
        )
    }
    pub(crate) fn list(&self, query: &str) -> Value {
        self.request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o?{query}"),
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .json_ok()
    }
    pub(crate) fn stop(&mut self) {
        self.0.stop();
    }
}
pub(crate) fn settings(dir: &common::TestDir, versioning: bool, hold: bool) {
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap().execute(
        "UPDATE buckets SET versioning_enabled = ?1, default_event_based_hold = ?2 WHERE name='test-bucket'", rusqlite::params![versioning, hold],
    ).unwrap();
}
pub(crate) fn request(
    address: &str,
    method: &str,
    path: &str,
    headers: &str,
    body: &[u8],
) -> Reply {
    Client {
        address,
        host: "localhost",
        headers: "Content-Type: application/json\r\n",
        timeout: Duration::from_secs(10),
    }
    .request_eof(method, path, headers, body)
    .into_joined()
}
pub(crate) fn blobs(dir: &common::TestDir) -> usize {
    common::process::blob_count(dir)
}
