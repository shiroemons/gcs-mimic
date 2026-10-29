use std::{
    io::BufRead,
    process::{Child, Command},
    time::{Duration, Instant},
};

#[path = "../../src/test_support.rs"]
pub(super) mod input_fixtures;
pub type TestDir = input_fixtures::TestDir;

/// PUT defaults to `projection=full`; remove ACL fields before comparing with noAcl reads.
#[allow(dead_code)]
pub fn without_acl(mut value: serde_json::Value) -> serde_json::Value {
    let resource = value.as_object_mut().unwrap();
    for field in ["acl", "defaultObjectAcl", "owner"] {
        resource.remove(field);
    }
    value
}

#[allow(dead_code)]
pub fn http_config() -> gcs_mimic::config::Config {
    gcs_mimic::config::Config {
        scheme: gcs_mimic::config::Scheme::Http,
        api_base_url: Some("http://127.0.0.1:4443".into()),
        ..Default::default()
    }
}

#[allow(dead_code)]
pub fn load_http_config(
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> Result<gcs_mimic::config::Config, gcs_mimic::diagnostics::Diagnostics> {
    let mut args = args.to_vec();
    args.extend(["--scheme".into(), "http".into()]);
    gcs_mimic::config::Config::load(&args, env)
}

#[allow(dead_code)]
#[path = "../../src/storage/test_support.rs"]
pub mod timestamp_support;

#[allow(dead_code)]
pub fn downgrade_timestamp_precision(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_timestamp_precision(connection);
}

#[allow(dead_code)]
pub fn downgrade_object_schema(connection: &rusqlite::Connection) {
    timestamp_support::downgrade_object_schema(connection);
}

/// Reads one HTTP/1.1 response as raw bytes, including any 1xx interim responses
/// before it. The status, Content-Length or chunked encoding frames it, because a
/// client on a persistent connection cannot wait for the server to close; `head`
/// marks a response to HEAD, which carries no body.
#[allow(dead_code)]
pub fn read_http_response(reader: &mut impl BufRead, head: bool) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Read};
    fn line(reader: &mut impl BufRead, raw: &mut Vec<u8>) -> std::io::Result<usize> {
        let start = raw.len();
        if reader.read_until(b'\n', raw)? == 0 {
            return Err(Error::from(ErrorKind::UnexpectedEof));
        }
        Ok(start)
    }
    let mut raw = Vec::new();
    let response_head = loop {
        let start = raw.len();
        while !raw[start..].ends_with(b"\r\n\r\n") {
            line(reader, &mut raw)?;
        }
        let response_head = String::from_utf8_lossy(&raw[start..]).to_ascii_lowercase();
        let status = response_head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse::<u16>().ok())
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing HTTP status"))?;
        if (100..200).contains(&status) {
            continue;
        }
        if head || status == 204 || status == 304 {
            return Ok(raw);
        }
        break response_head;
    };
    let header = |name: &str| {
        response_head
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .map(str::trim)
    };
    if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        loop {
            let start = line(reader, &mut raw)?;
            let size = String::from_utf8_lossy(&raw[start..]);
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
            if size == 0 {
                while line(reader, &mut raw).map(|start| raw[start..] != *b"\r\n")? {}
                return Ok(raw);
            }
            let start = raw.len();
            raw.resize(start + size + 2, 0);
            reader.read_exact(&mut raw[start..])?;
        }
    }
    match header("content-length") {
        Some(length) => {
            let length = length
                .parse::<u64>()
                .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
            if reader.by_ref().take(length).read_to_end(&mut raw)? as u64 != length {
                return Err(Error::from(ErrorKind::UnexpectedEof));
            }
        }
        None => {
            reader.read_to_end(&mut raw)?;
        }
    }
    Ok(raw)
}

/// macOS reports a peer close during transfer as a reset or broken pipe where
/// Linux often still accepts the write or returns EOF.
#[allow(dead_code)]
pub fn closed_by_server(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

/// Starts a child from an empty environment, keeping only what the platform
/// needs to create a process: Windows cannot initialize Winsock without
/// `SystemRoot`, so servers exited before readiness there.
#[allow(dead_code)]
pub trait CleanEnv {
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

pub fn server_command() -> Command {
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

pub fn stop_server(child: &mut Child, stdout: &mut impl BufRead) {
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
