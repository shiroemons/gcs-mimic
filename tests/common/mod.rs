#![expect(
    dead_code,
    reason = "Integration binaries use different subsets of the harness."
)]

use std::{
    io::BufRead,
    process::{Child, Command},
    time::{Duration, Instant},
};

pub(crate) mod catalog;
pub(crate) mod effects;
pub(crate) mod errors;
pub(crate) mod http;
pub(crate) mod ingress;
pub(crate) mod interrupt;
pub(crate) mod notifications;
pub(crate) mod process;

// Unit and integration tests share one fixture source so cleanup behavior cannot drift.
#[path = "../../src/test_support.rs"]
pub(super) mod input_fixtures;
pub(crate) type TestDir = input_fixtures::TestDir;
// Only `eventually` is used by every binary (through `process::no_leaks`); an unused
// re-export warns per binary, so the other waits are reached through `input_fixtures`.
pub(crate) use input_fixtures::eventually;

/// PUT defaults to `projection=full`; remove ACL fields before comparing with noAcl reads.
pub(crate) fn without_acl(mut value: serde_json::Value) -> serde_json::Value {
    let resource = value.as_object_mut().unwrap();
    for field in ["acl", "defaultObjectAcl", "owner"] {
        resource.remove(field);
    }
    value
}

pub(crate) fn http_config() -> gcs_mimic::config::Config {
    gcs_mimic::config::Config {
        scheme: gcs_mimic::config::Scheme::Http,
        api_base_url: Some("http://127.0.0.1:4443".into()),
        ..Default::default()
    }
}

pub(crate) fn load_http_config(
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> Result<gcs_mimic::config::Config, gcs_mimic::diagnostics::Diagnostics> {
    let mut args = args.to_vec();
    args.extend(["--scheme".into(), "http".into()]);
    gcs_mimic::config::Config::load(&args, env)
}

// Schema downgrade fixtures must track the private catalog schema in one source.
#[path = "../../src/storage/catalog/test_support.rs"]
pub(crate) mod timestamp_support;

pub(crate) fn downgrade_timestamp_precision(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_timestamp_precision(connection);
}

pub(crate) fn downgrade_deletion_history_schema(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_deletion_history_schema(connection);
}

pub(crate) fn downgrade_object_schema(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_object_schema(connection);
}

/// Reads one HTTP/1.1 response as raw bytes, including any 1xx interim responses
/// before it. The status, Content-Length or chunked encoding frames it, because a
/// client on a persistent connection cannot wait for the server to close. Responses
/// without framing are read until EOF; `Body::Head` suppresses the response body.
pub(crate) fn read_http_response(
    reader: &mut impl BufRead,
    body: http::Body,
) -> std::io::Result<Vec<u8>> {
    http::read_eof(reader, body).map(|response| response.raw)
}

/// macOS reports a peer close during transfer as a reset or broken pipe where
/// Linux often still accepts the write or returns EOF.
pub(crate) fn closed_by_server(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

/// Waits for the server to close a connection whose response announced
/// `Connection: close`, so the client never sends the first FIN. The side that
/// closes first keeps the address pair in TIME_WAIT (30 s on macOS); on the
/// client that holds an ephemeral port, and harnesses opening one connection per
/// request used up the range under concurrent suites, failing connects with
/// `AddrNotAvailable` and servers binding port 0. The response is already read,
/// so an error here only means the server reset input it did not read.
pub(crate) fn await_server_close(reader: &mut impl std::io::Read) {
    let _ = std::io::copy(reader, &mut std::io::sink());
}

/// Starts a child from an empty environment, keeping only what the platform
/// needs to create a process: Windows cannot initialize Winsock without
/// `SystemRoot`, so servers exited before readiness there.
pub(crate) trait CleanEnv {
    fn clean_env(&mut self) -> &mut Self;
}

impl CleanEnv for Command {
    fn clean_env(&mut self) -> &mut Self {
        self.env_clear();
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            self.env("SystemRoot", root);
        }
        self
    }
}

/// The header line that `gcs-mimic sync` sends with every control request and the server sends
/// with every control answer: each side refuses a peer of another release.
pub(crate) fn sync_version_header() -> String {
    format!("x-gcs-mimic-version: {}\r\n", gcs_mimic::VERSION)
}

pub(crate) fn server_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"));
    command.clean_env();
    #[cfg(windows)]
    {
        use std::{os::windows::process::CommandExt, sync::Once};
        use windows_sys::Win32::System::{
            Console::{AllocConsole, GetConsoleCP},
            Threading::CREATE_NEW_PROCESS_GROUP,
        };

        static CONSOLE: Once = Once::new();
        CONSOLE.call_once(|| {
            // CI may have no console; children must share one to receive control events.
            #[expect(
                unsafe_code,
                reason = "Windows tests need to discover a shared console for child control events."
            )]
            // SAFETY: Reading the current console code page takes no pointers or handles.
            let code_page = unsafe { GetConsoleCP() };
            if code_page == 0 {
                #[expect(
                    unsafe_code,
                    reason = "Allocate a shared console exactly once for Windows test children."
                )]
                // SAFETY: This pointer-free call is serialized by Once when no console exists.
                let allocated = unsafe { AllocConsole() };
                assert_ne!(allocated, 0, "{}", std::io::Error::last_os_error());
            }
        });
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    command
}

pub(crate) fn stop_server(child: &mut Child, stdout: &mut impl BufRead) {
    #[cfg(unix)]
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};

        #[expect(
            unsafe_code,
            reason = "Windows test shutdown targets only the child's private process group."
        )]
        // SAFETY: The child's nonzero process group confines CTRL-BREAK to this child.
        let signaled = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
        assert_ne!(signaled, 0, "{}", std::io::Error::last_os_error());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("server did not stop before the shutdown deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "server shutdown status: {status}");
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let stopped: serde_json::Value = serde_json::from_str(&line).expect("graceful shutdown event");
    assert_eq!(stopped["event"], "stopped");
}
