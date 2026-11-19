// Directory synchronization supports only Linux and macOS; elsewhere roots are rejected.
#![cfg(any(target_os = "linux", target_os = "macos"))]

//! Switching a data directory to an empty directory in its synchronized root's place, as a
//! container sees it when its bind mount changes.

use crate::common;
use common::TestDir;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Child, ChildStdout, Stdio},
    sync::mpsc,
};

/// Enough files for removing them all to wait for approval instead of deleting them.
const FILES: usize = 100;

struct Running {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
    events: mpsc::Receiver<Value>,
}

/// Starts a server over `tree`, or returns what it wrote to standard error when startup fails.
fn start(data: &TestDir, tree: &Path) -> Result<Running, String> {
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--seed-project",
            "local",
            "--data-dir",
        ])
        .arg(&data.0)
        .arg("--sync-dir")
        .arg(tree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    if stdout.read_line(&mut line).unwrap() == 0 {
        let mut error = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert!(!child.wait().unwrap().success(), "{error}");
        return Err(error);
    }
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["event"], "ready", "{line}");
    let stderr = child.stderr.take().unwrap();
    let (send, events) = mpsc::channel();
    // Draining standard error keeps a full pipe from stalling the server.
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if send.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    Ok(Running {
        child,
        stdout,
        address: ready["listen"].as_str().unwrap().to_owned(),
        events,
    })
}

impl Running {
    fn cli(&self, command: &str, options: &[&str], success: bool) -> Value {
        let output = common::server_command()
            .args(["sync", command, "--endpoint"])
            .arg(format!("http://{}", self.address))
            .args(options)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{command} {options:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Rescans and waits until the root is ready, which it is not while a finding or a deletion
    /// waiting for approval remains.
    fn settle(&self) {
        self.cli("rescan", &["--timeout", "60s"], true);
        assert_eq!(
            self.cli("wait", &["--timeout", "10s"], true)["completed"],
            true
        );
    }

    /// The bodies of the objects `test-bucket` holds, by name.
    fn objects(&self) -> Vec<(String, Vec<u8>)> {
        let list = common::effects::request(
            &self.address,
            "GET",
            "/storage/v1/b/test-bucket/o?maxResults=1000",
            "",
            b"",
        );
        assert_eq!(list.status, 200, "{}", String::from_utf8_lossy(&list.body));
        let list: Value = serde_json::from_slice(&list.body).unwrap();
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                let name = item["name"].as_str().unwrap().to_owned();
                let path = format!("/storage/v1/b/test-bucket/o/{name}?alt=media");
                let body = common::effects::request(&self.address, "GET", &path, "", b"");
                assert_eq!(body.status, 200, "{name}");
                (name, body.body)
            })
            .collect()
    }

    /// Stops the server and returns the events it logged.
    fn stop(mut self) -> Vec<Value> {
        common::stop_server(&mut self.child, &mut self.stdout);
        self.events.iter().collect()
    }
}

fn names() -> Vec<String> {
    (0..FILES).map(|index| format!("{index:03}")).collect()
}

fn marker_root(tree: &Path) -> String {
    let marker: Value =
        serde_json::from_slice(&fs::read(tree.join(".gcs-mimic-sync.json")).unwrap()).unwrap();
    marker["root_id"].as_str().unwrap().to_owned()
}

/// Starts over `tree`, which must take the synchronized root's place, and checks that every
/// object is written into it, none is deleted and nothing waits for approval. Returns the
/// warning that startup logged about the switch.
fn switch_to(data: &TestDir, tree: &Path) -> Value {
    let running = start(data, tree).unwrap();
    running.settle();
    assert_eq!(
        running.cli("status", &[], true)["pending_deletion_count"],
        0
    );
    let objects = running.objects();
    assert_eq!(
        objects
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        names()
    );
    for (name, body) in objects {
        assert_eq!(body, b"body", "{name}");
        assert_eq!(
            fs::read(tree.join("test-bucket").join(&name)).unwrap(),
            b"body"
        );
    }
    let events = running.stop();
    let switched = events
        .iter()
        .find(|event| event["event"] == "sync_root_switched")
        .unwrap_or_else(|| panic!("no switch logged: {events:?}"))
        .clone();
    assert_eq!(switched["level"], "warn", "{switched}");
    assert_eq!(switched["reason"], "empty_directory", "{switched}");
    assert_eq!(switched["root_id"], marker_root(tree), "{switched}");
    switched
}

/// A directory that takes the synchronized root's place, as a changed bind mount does, starts
/// synchronizing when it is empty or holds only `.DS_Store`. Every object is written into it and
/// none is deleted, although the previous tree had lost every file and its deletions were waiting
/// for approval: the catalog forgets that tree's tracked files and candidates. A directory that
/// holds anything else is refused with a remedy that names the switch, and so is the previous
/// tree put back, which is not empty. Startup never changes the trees it leaves or refuses.
#[test]
fn an_empty_directory_in_the_root_place_takes_every_object_and_the_previous_tree_stays() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let aside = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for name in names() {
        fs::write(tree.0.join("test-bucket").join(name), b"body").unwrap();
    }
    let running = start(&data, &tree.0).unwrap();
    running.settle();
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    running.cli("rescan", &[], true);
    let status = running.cli("status", &[], true);
    assert_eq!(status["state"], "DeletionApproval", "{status}");
    assert_eq!(status["pending_deletion_count"], FILES);
    running.stop();
    let previous = aside.0.join("previous");
    fs::rename(&tree.0, &previous).unwrap();
    let previous_marker = fs::read(previous.join(".gcs-mimic-sync.json")).unwrap();
    let mut previous_root = marker_root(&previous);

    for finder in [false, true] {
        fs::create_dir(&tree.0).unwrap();
        if finder {
            fs::write(tree.0.join(".DS_Store"), b"finder").unwrap();
        }
        let switched = switch_to(&data, &tree.0);
        assert_eq!(switched["previous_root_id"], previous_root, "{switched}");
        assert_eq!(switched["forgotten_tracked_files"], FILES, "{switched}");
        let candidates = if finder { 0 } else { FILES };
        assert_eq!(
            switched["forgotten_deletion_candidates"], candidates,
            "{switched}"
        );
        assert_eq!(switched["finder_metadata"], usize::from(finder));
        if finder {
            assert_eq!(fs::read(tree.0.join(".DS_Store")).unwrap(), b"finder");
        }
        previous_root = marker_root(&tree.0);
        fs::rename(&tree.0, aside.0.join(format!("switched-{finder}"))).unwrap();
    }
    fs::rename(aside.0.join("switched-true"), &tree.0).unwrap();

    let refused = aside.0.join("refused");
    fs::create_dir(&refused).unwrap();
    fs::write(refused.join("notes.txt"), b"mine").unwrap();
    fs::rename(&tree.0, aside.0.join("current")).unwrap();
    for candidate in [&refused, &previous] {
        fs::rename(candidate, &tree.0).unwrap();
        let error = start(&data, &tree.0).err().unwrap();
        assert!(error.contains("RootChanged"), "{error}");
        assert!(error.contains("selecting an empty one"), "{error}");
        assert!(!tree.0.join(".gcs-mimic-sync.json.staging").exists());
        fs::rename(&tree.0, candidate).unwrap();
    }
    assert_eq!(fs::read(refused.join("notes.txt")).unwrap(), b"mine");
    assert!(!refused.join(".gcs-mimic-sync.json").exists());
    assert_eq!(
        fs::read(previous.join(".gcs-mimic-sync.json")).unwrap(),
        previous_marker
    );
    assert!(!previous.join("test-bucket").exists());

    // The directory the catalog synchronizes with now still opens.
    fs::rename(aside.0.join("current"), &tree.0).unwrap();
    let running = start(&data, &tree.0).unwrap();
    running.settle();
    let events = running.stop();
    assert!(
        events
            .iter()
            .all(|event| event["event"] != "sync_root_switched"),
        "{events:?}"
    );
}
