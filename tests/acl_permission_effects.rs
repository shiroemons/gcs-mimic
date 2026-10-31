mod object_effects_support;

use object_effects_support::{
    Reply,
    common::{self, TestDir},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    process::{Child, ChildStdout, Stdio},
};

mod acl_permission_effects {
    use super::*;
    mod bidi;
    mod bucket;
    mod compatibility;
    mod derived;
    mod grpc;
    mod mirror;
    mod owner;
    mod owner_authority;
    mod owner_publication;
    mod owner_replay;
    mod read;
}

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const READER: &str = "Authorization: Bearer reader\r\n";
const OTHER: &str = "Authorization: Bearer other\r\n";
const BUCKET: &str = "/storage/v1/b/test-bucket";
const OBJECT: &str = "/storage/v1/b/test-bucket/o/value";

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
    grpc_address: String,
}

impl Server {
    fn start(data: &TestDir, inputs: &TestDir, backend: &str) -> Self {
        Self::start_with_registry(
            data,
            inputs,
            backend,
            &json!({"identities":[{"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},{"id":"reader@example.test","tokens":["reader"]},{"id":"other@example.test","tokens":["other"]}]}),
        )
    }
    fn start_with_registry(
        data: &TestDir,
        inputs: &TestDir,
        backend: &str,
        registry: &Value,
    ) -> Self {
        let identities = inputs.0.join("identities.json");
        std::fs::write(&identities, registry.to_string()).unwrap();
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--grpc-listen",
                "127.0.0.1:0",
                "--api-base-url",
                "http://127.0.0.1:4443",
                "--seed-project",
                "local",
                "--auth-mode",
                "strict",
                "--backend",
                backend,
                "--identity-file",
            ])
            .arg(identities)
            .arg("--data-dir")
            .arg(&data.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Self {
            child,
            stdout,
            address: String::new(),
            grpc_address: String::new(),
        };
        let mut line = String::new();
        server.stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        server.address = ready["listen"].as_str().unwrap().to_owned();
        server.grpc_address = ready["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .find(|listener| listener["scheme"] == "h2c")
            .unwrap()["listen"]
            .as_str()
            .unwrap()
            .to_owned();
        server
    }
    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        object_effects_support::request(&self.address, method, path, headers, body)
    }
    fn admin(&self, method: &str, path: &str, body: Value) -> Reply {
        self.request(method, path, ADMIN, body.to_string().as_bytes())
    }
    fn bucket(&self) {
        self.admin(
            "POST",
            "/storage/v1/b?project=local",
            json!({"name":"test-bucket","versioning":{"enabled":true}}),
        )
        .json();
    }
    fn put(&self, body: &[u8]) -> Value {
        self.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            ADMIN,
            body,
        )
        .json()
    }
    fn grant(&self, entity: &str, role: &str, generation: &str) {
        self.admin(
            "POST",
            &format!("{OBJECT}/acl?generation={generation}"),
            json!({"entity":entity,"role":role}),
        )
        .json();
    }
    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        // Cleanup must reap the child even while a failed assertion is unwinding.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
