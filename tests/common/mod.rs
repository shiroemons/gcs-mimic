#![expect(
    dead_code,
    reason = "Integration binaries use different subsets of the harness."
)]

use std::{
    io::BufRead,
    process::{Child, Command},
    time::{Duration, Instant},
};

pub(crate) mod effects;
pub(crate) mod errors;
pub(crate) mod http;
pub(crate) mod ingress;
pub(crate) mod server;

#[expect(
    dead_code,
    reason = "Each integration suite uses a subset of the shared fixtures"
)]
pub(super) mod input_fixtures;
pub(crate) type TestDir = input_fixtures::TestDir;

/// PUT defaults to `projection=full`; remove ACL fields before comparing with noAcl reads.
#[allow(dead_code)]
pub(crate) fn without_acl(mut value: serde_json::Value) -> serde_json::Value {
    let resource = value.as_object_mut().unwrap();
    for field in ["acl", "defaultObjectAcl", "owner"] {
        resource.remove(field);
    }
    value
}

#[allow(dead_code)]
pub(crate) fn http_config() -> gcs_mimic::config::Config {
    gcs_mimic::config::Config {
        scheme: gcs_mimic::config::Scheme::Http,
        api_base_url: Some("http://127.0.0.1:4443".into()),
        ..Default::default()
    }
}

#[allow(dead_code)]
pub(crate) fn load_http_config(
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> Result<gcs_mimic::config::Config, gcs_mimic::diagnostics::Diagnostics> {
    let mut args = args.to_vec();
    args.extend(["--scheme".into(), "http".into()]);
    gcs_mimic::config::Config::load(&args, env)
}

#[allow(dead_code)]
// Schema downgrade fixtures track private catalog internals; keep one source until it is public.
#[path = "../../src/storage/catalog/test_support.rs"]
pub(crate) mod timestamp_support;

#[allow(dead_code)]
pub(crate) fn downgrade_timestamp_precision(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_timestamp_precision(connection);
}

#[allow(dead_code)]
pub(crate) fn downgrade_object_schema(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_object_schema(connection);
}

/// Reads one HTTP/1.1 response as raw bytes, including any 1xx interim responses
/// before it. The status, Content-Length or chunked encoding frames it, because a
/// client on a persistent connection cannot wait for the server to close; `head`
/// marks a response to HEAD, which carries no body.
#[allow(dead_code)]
pub(crate) fn read_http_response(
    reader: &mut impl BufRead,
    head: bool,
) -> std::io::Result<Vec<u8>> {
    http::read(reader, head).map(|response| response.raw)
}

/// macOS reports a peer close during transfer as a reset or broken pipe where
/// Linux often still accepts the write or returns EOF.
#[allow(dead_code)]
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
#[allow(dead_code)]
pub(crate) fn await_server_close(reader: &mut impl std::io::Read) {
    let _ = std::io::copy(reader, &mut std::io::sink());
}

/// Starts a child from an empty environment, keeping only what the platform
/// needs to create a process: Windows cannot initialize Winsock without
/// `SystemRoot`, so servers exited before readiness there.
#[allow(dead_code)]
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

pub(crate) fn server_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"));
    command.clean_env();
    // New buckets get Cloud Storage's seven-day soft delete by default. Most tests exercise
    // permanent deletion and exact blob counts, so the suite's servers start with the default
    // off; a test that studies the default passes the CLI option, which outranks this variable.
    command.env("GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS", "0");
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
            // SAFETY: Reading the current console code page takes no pointers or handles.
            #[allow(unsafe_code)]
            let code_page = unsafe { GetConsoleCP() };
            if code_page == 0 {
                // SAFETY: This pointer-free call is serialized by Once when no console exists.
                #[allow(unsafe_code)]
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

        // SAFETY: The child's nonzero process group confines CTRL-BREAK to this child.
        #[allow(unsafe_code)]
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
