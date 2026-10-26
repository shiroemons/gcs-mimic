use std::{
    io::BufRead,
    process::{Child, Command},
    time::{Duration, Instant},
};

#[path = "../../src/test_support.rs"]
pub(super) mod input_fixtures;
pub type TestDir = input_fixtures::TestDir;

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

pub fn server_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"));
    command.env_clear();
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
