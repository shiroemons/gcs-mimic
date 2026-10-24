use std::{
    fs,
    io::BufRead,
    path::PathBuf,
    process::{Child, Command},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub struct TestDir(pub PathBuf);

#[allow(dead_code)]
pub fn http_config() -> gcs_mimic::config::Config {
    gcs_mimic::config::Config {
        scheme: gcs_mimic::config::Scheme::Http,
        api_base_url: "http://127.0.0.1:4443".into(),
        ..Default::default()
    }
}

#[allow(dead_code)]
pub fn load_http_config(
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> Result<gcs_mimic::config::Config, String> {
    let mut args = args.to_vec();
    args.extend(["--scheme".into(), "http".into()]);
    gcs_mimic::config::Config::load(&args, env)
}

#[allow(dead_code)]
pub fn downgrade_object_schema(connection: &rusqlite::Connection) {
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN public_access_prevention_enforced; ALTER TABLE buckets DROP COLUMN cors; DROP TABLE rewrite_sessions; DROP TABLE derived_deletions; DROP TABLE derived_inputs; DROP TABLE derived_transfers;
        ALTER TABLE objects RENAME TO objects_with_versions;
        CREATE TABLE objects (
            bucket TEXT NOT NULL COLLATE BINARY,
            name TEXT NOT NULL COLLATE BINARY,
            generation INTEGER NOT NULL CHECK(generation > 0),
            metageneration INTEGER NOT NULL CHECK(metageneration > 0),
            content_type TEXT NOT NULL,
            size INTEGER NOT NULL CHECK(size >= 0),
            md5 BLOB NOT NULL CHECK(length(md5) = 16),
            crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
            storage_class TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            updated INTEGER NOT NULL,
            blob_id TEXT NOT NULL UNIQUE,
            PRIMARY KEY(bucket, name)
        ) WITHOUT ROWID;
        ALTER TABLE objects ADD COLUMN content_encoding TEXT;
        ALTER TABLE objects ADD COLUMN content_disposition TEXT;
        ALTER TABLE objects ADD COLUMN content_language TEXT;
        ALTER TABLE objects ADD COLUMN cache_control TEXT;
        ALTER TABLE objects ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
        INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata) SELECT bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata FROM objects_with_versions;
        DROP TABLE objects_with_versions;").unwrap();
}

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
