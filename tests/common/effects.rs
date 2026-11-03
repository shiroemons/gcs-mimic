#![expect(
    dead_code,
    reason = "Integration binaries use different object-effect scenarios."
)]

pub(crate) use crate::common;
use common::http::Client;
pub(crate) use common::http::Reply;
use serde_json::Value;
use std::time::Duration;

common::server::declare_server!();
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
        let mut command = common::server::http_command(&dir.0);
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
        let mut command = common::server::http_command(&dir.0);
        command
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .arg("--catalog-wait-timeout-ms")
            .arg(catalog_milliseconds.to_string())
            .arg("--object-request-timeout-ms")
            .arg(request_milliseconds.to_string())
            .arg("--max-object-bytes")
            .arg(max_object_bytes.to_string());
        if strict {
            let identities = dir.0.join("identities.json");
            std::fs::write(&identities, serde_json::json!({"identities":[
                {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local":["roles/storage.objectViewer"]}},
                {"id":"creator@example.test","tokens":["creator"],"project_roles":{"local":["roles/storage.objectCreator"]}},
                {"id":"scoped@example.test","tokens":["scoped"]}
            ]}).to_string()).unwrap();
            command
                .args(["--auth-mode", "strict", "--identity-file"])
                .arg(identities);
        }
        Self::launch(&mut command)
    }
    pub(crate) fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, headers, body)
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
    .request(method, path, headers, body)
    .into_joined()
}
pub(crate) fn blobs(dir: &common::TestDir) -> usize {
    common::server::blob_count(dir)
}
