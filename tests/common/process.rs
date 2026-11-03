#![expect(
    dead_code,
    reason = "Each integration binary uses a different subset of the harness."
)]

use super::{TestDir, server_command, stop_server};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

// Each suite adds scenario-specific methods without extending another suite's server.
macro_rules! declare_server {
    () => {
        pub(crate) struct Server(crate::common::server::Server);
        impl std::ops::Deref for Server {
            type Target = crate::common::server::Server;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
        impl std::ops::DerefMut for Server {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }
        impl Server {
            fn launch(command: &mut std::process::Command) -> Self {
                Self(crate::common::server::Server::launch(command))
            }
        }
    };
}
pub(crate) use declare_server;

pub(crate) fn write_request(
    stream: &mut impl Write,
    method: &str,
    path: &str,
    host: &str,
    headers: &str,
    body: &[u8],
) {
    stream
        .write_all(request_head(method, path, host, headers, body.len()).as_bytes())
        .unwrap();
    stream.write_all(body).unwrap();
}

pub(crate) fn request_head(
    method: &str,
    path: &str,
    host: &str,
    headers: &str,
    length: usize,
) -> String {
    format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {length}\r\n{headers}\r\n")
}

pub(crate) use super::http::Reply;

impl Reply {
    pub(crate) fn framed(stream: std::net::TcpStream) -> Self {
        super::http::receive(stream, false).into_last()
    }
}

pub(crate) struct Server {
    pub(crate) child: Child,
    pub(crate) address: String,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) ready: Value,
    pub(crate) client_timeout: Duration,
    shutdown: Shutdown,
}

enum Shutdown {
    Kill,
    Graceful,
}

pub(crate) fn http_command(data: &Path) -> Command {
    serve("127.0.0.1:0", data)
}

pub(crate) fn serve(listen: &str, data: &Path) -> Command {
    let mut command = server_command();
    command
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            listen,
            "--data-dir",
        ])
        .arg(data);
    command
}

pub(crate) fn write_identities(dir: &Path, identities: &Value) -> std::path::PathBuf {
    let path = dir.join("identities.json");
    std::fs::write(&path, identities.to_string()).unwrap();
    path
}

pub(crate) fn strict_args(dir: &Path, identities: &Value) -> [std::ffi::OsString; 4] {
    [
        "--auth-mode".into(),
        "strict".into(),
        "--identity-file".into(),
        write_identities(dir, identities).into_os_string(),
    ]
}

impl Server {
    pub(crate) fn pending(child: Child, stdout: BufReader<ChildStdout>, address: String) -> Self {
        Self {
            address,
            child,
            stdout,
            ready: Value::Null,
            client_timeout: Duration::from_secs(5),
            shutdown: Shutdown::Kill,
        }
    }

    pub(crate) fn stop(&mut self) {
        stop_server(&mut self.child, &mut self.stdout);
    }

    pub(crate) fn launch(command: &mut Command) -> Self {
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event after recovery");
        assert_eq!(ready["event"], "ready", "{ready}");
        let mut server = Self::pending(child, stdout, ready["listen"].as_str().unwrap().into());
        server.ready = ready;
        server
    }

    pub(crate) fn graceful(mut self) -> Self {
        self.shutdown = Shutdown::Graceful;
        self
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return;
        }
        match self.shutdown {
            Shutdown::Graceful => stop_server(&mut self.child, &mut self.stdout),
            Shutdown::Kill if self.child.try_wait().unwrap().is_none() => {
                self.child.kill().unwrap();
                self.child.wait().unwrap();
            }
            Shutdown::Kill => {}
        }
    }
}

pub(crate) fn staging_count(dir: &TestDir) -> usize {
    std::fs::read_dir(dir.0.join("staging")).unwrap().count()
}

pub(crate) fn blob_count(dir: &TestDir) -> usize {
    std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

pub(crate) fn no_leaks(dir: &TestDir, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let staging = staging_count(dir);
        let blobs = blob_count(dir);
        if staging == 0 && blobs == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "staging={staging}, blobs={blobs}, expected={expected}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
