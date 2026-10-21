use std::{
    fs,
    io::BufRead,
    path::PathBuf,
    process::{Child, Command},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "gcs-mimic-wire-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
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
            if unsafe { GetConsoleCP() } == 0 {
                assert_ne!(
                    unsafe { AllocConsole() },
                    0,
                    "{}",
                    std::io::Error::last_os_error()
                );
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

        // A nonzero process group confines CTRL-BREAK to this child, unlike CTRL-C.
        assert_ne!(
            unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
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
