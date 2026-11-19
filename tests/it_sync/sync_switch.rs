// Directory synchronization supports only Linux and macOS; elsewhere roots are rejected.
#![cfg(any(target_os = "linux", target_os = "macos"))]

//! A data directory synchronizes one directory. Startup given another directory switches to it
//! when it is empty, merges it when it holds this data directory's marker, and refuses it
//! otherwise; none of these deletes an object or a file, and only the directory it synchronizes
//! is compared with the files the catalog tracks.

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

    /// The objects `test-bucket` holds, by name, with their generations and bodies.
    fn objects(&self) -> Vec<(String, String, Vec<u8>)> {
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
                let generation = item["generation"].as_str().unwrap().to_owned();
                (name, generation, body.body)
            })
            .collect()
    }

    fn upload(&self, name: &str, body: &[u8]) {
        let path = format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name={name}");
        let response = common::effects::request(&self.address, "POST", &path, "", body);
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
    }

    fn delete(&self, name: &str) {
        let path = format!("/storage/v1/b/test-bucket/o/{name}");
        let response = common::effects::request(&self.address, "DELETE", &path, "", b"");
        assert_eq!(response.status, 204);
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

fn canonical(tree: &Path) -> String {
    fs::canonicalize(tree)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The names and bodies of the files in the bucket directory of `tree`.
fn files(tree: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = fs::read_dir(tree.join("test-bucket")) else {
        return Vec::new();
    };
    let mut files = entries
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// The one change warning among `events`, which must report that the data directory now
/// synchronizes `tree` for `reason`, instead of each of `previous`.
fn switched(events: &[Value], reason: &str, previous: &[&Path], tree: &Path) -> Value {
    let mut changes = events
        .iter()
        .filter(|event| event["event"] == "sync_root_switched");
    let change = changes
        .next()
        .unwrap_or_else(|| panic!("no switch logged: {events:?}"))
        .clone();
    assert!(changes.next().is_none(), "{events:?}");
    assert_eq!(change["level"], "warn", "{change}");
    assert_eq!(change["reason"], reason, "{change}");
    assert_eq!(change["path"], canonical(tree), "{change}");
    assert_eq!(change["root_id"], marker_root(tree), "{change}");
    let paths = change["previous_roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|root| root["path"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let expected = previous
        .iter()
        .map(|path| canonical(path))
        .collect::<Vec<_>>();
    assert_eq!(paths, expected, "{change}");
    assert!(
        change["remedy"]
            .as_str()
            .is_some_and(|remedy| !remedy.is_empty())
    );
    change
}

/// Starts over `tree`, which must take the synchronized root's place, and checks that every
/// object is written into it, none is deleted and nothing waits for approval. Returns the
/// warning that startup logged about the switch.
fn switch_to(data: &TestDir, tree: &Path, previous: &Path) -> Value {
    let running = start(data, tree).unwrap();
    running.settle();
    let status = running.cli("status", &[], true);
    assert_eq!(status["pending_deletion_count"], 0);
    assert_eq!(status["root_path"], canonical(tree), "{status}");
    assert_eq!(
        status["root_change"]["reason"], "empty_directory",
        "{status}"
    );
    let objects = running.objects();
    assert_eq!(
        objects
            .iter()
            .map(|(name, ..)| name.clone())
            .collect::<Vec<_>>(),
        names()
    );
    for (name, _, body) in objects {
        assert_eq!(body, b"body", "{name}");
        assert_eq!(
            fs::read(tree.join("test-bucket").join(&name)).unwrap(),
            b"body"
        );
    }
    let events = running.stop();
    switched(&events, "empty_directory", &[previous], tree)
}

/// A directory that takes the synchronized root's place, as a changed bind mount does, starts
/// synchronizing when it holds no subdirectory, only files directly in it such as `.DS_Store`.
/// Every object is written into it and none is deleted, although the previous tree had lost every
/// file and its deletions were waiting for approval: the catalog forgets that tree's tracked files
/// and candidates. A directory that holds a subdirectory is refused with a remedy that names the
/// synchronized directory. Startup never changes the trees it leaves or refuses.
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
        let switched = switch_to(&data, &tree.0, &tree.0);
        assert_eq!(
            switched["previous_roots"][0]["root_id"], previous_root,
            "{switched}"
        );
        assert_eq!(switched["forgotten_tracked_files"], FILES, "{switched}");
        let candidates = if finder { 0 } else { FILES };
        assert_eq!(
            switched["forgotten_deletion_candidates"], candidates,
            "{switched}"
        );
        assert_eq!(switched["root_files"], usize::from(finder));
        if finder {
            assert_eq!(fs::read(tree.0.join(".DS_Store")).unwrap(), b"finder");
        }
        previous_root = marker_root(&tree.0);
        fs::rename(&tree.0, aside.0.join(format!("switched-{finder}"))).unwrap();
    }
    fs::rename(aside.0.join("switched-true"), &tree.0).unwrap();
    let current = canonical(&tree.0);

    let refused = aside.0.join("refused");
    fs::create_dir_all(refused.join("test-bucket")).unwrap();
    fs::write(refused.join("test-bucket/notes.txt"), b"mine").unwrap();
    fs::rename(&tree.0, aside.0.join("current")).unwrap();
    fs::rename(&refused, &tree.0).unwrap();
    let error = start(&data, &tree.0).err().unwrap();
    assert!(error.contains("ReplacementOccupied"), "{error}");
    assert!(error.contains(&current), "{error}");
    assert!(error.contains("select an empty directory"), "{error}");
    assert!(!tree.0.join(".gcs-mimic-sync.json.staging").exists());
    assert!(!tree.0.join(".gcs-mimic-sync.json").exists());
    fs::rename(&tree.0, &refused).unwrap();
    assert_eq!(
        fs::read(refused.join("test-bucket/notes.txt")).unwrap(),
        b"mine"
    );
    assert_eq!(
        fs::read(previous.join(".gcs-mimic-sync.json")).unwrap(),
        previous_marker
    );
    assert!(!previous.join("test-bucket").exists());

    // The directory the catalog synchronizes with now still opens.
    fs::rename(aside.0.join("current"), &tree.0).unwrap();
    let running = start(&data, &tree.0).unwrap();
    running.settle();
    assert!(running.cli("status", &[], true)["root_change"].is_null());
    let events = running.stop();
    assert!(
        events
            .iter()
            .all(|event| event["event"] != "sync_root_switched"),
        "{events:?}"
    );
}

/// Starts over a new tree whose bucket directory holds `x`, `y` and `z`, waits until the catalog
/// tracks them, and returns the objects.
fn synchronized(data: &TestDir, tree: &Path) -> Vec<(String, String, Vec<u8>)> {
    fs::create_dir(tree.join("test-bucket")).unwrap();
    for name in ["x", "y", "z"] {
        fs::write(tree.join("test-bucket").join(name), b"v1").unwrap();
    }
    let running = start(data, tree).unwrap();
    running.settle();
    let objects = running.objects();
    running.stop();
    objects
}

/// A data directory switched from A to an empty B at another path, where objects then change
/// through the API: `x` gets a second version, `new` is created and `y` is deleted. Started with
/// A again, startup merges A, which holds this data directory's marker, without comparing it with
/// the files it tracked in B: the second version of `x` stays and A's first version is kept as a
/// conflict, `new` is written into A, and nothing is deleted. A's file brings `y` back. The switch
/// to B and the merge of A each log the change with both paths, and leave the other tree as it
/// was; a later switch lists the conflict it forgets with the path of its bytes.
#[test]
fn returning_to_a_previous_directory_merges_it_and_deletes_nothing() {
    let data = TestDir::new();
    let (a, b, c) = (TestDir::new(), TestDir::new(), TestDir::new());
    let first = synchronized(&data, &a.0);
    let a_marker = fs::read(a.0.join(".gcs-mimic-sync.json")).unwrap();
    let a_files = files(&a.0);

    let running = start(&data, &b.0).unwrap();
    running.settle();
    assert_eq!(running.objects(), first);
    assert_eq!(files(&b.0), a_files);
    let status = running.cli("status", &[], true);
    assert_eq!(status["root_path"], canonical(&b.0), "{status}");
    assert_eq!(
        status["root_change"],
        serde_json::json!({"reason": "empty_directory", "previous_paths": [canonical(&a.0)]})
    );
    running.upload("x", b"v2");
    running.upload("new", b"new");
    running.delete("y");
    running.settle();
    let changed = running.objects();
    assert_eq!(
        files(&b.0),
        [
            ("new".to_owned(), b"new".to_vec()),
            ("x".to_owned(), b"v2".to_vec()),
            ("z".to_owned(), b"v1".to_vec()),
        ]
    );
    let events = running.stop();
    nothing_deleted_logged(&events);
    switched(&events, "empty_directory", &[a.0.as_path()], &b.0);
    assert_eq!(files(&a.0), a_files);
    assert_eq!(
        fs::read(a.0.join(".gcs-mimic-sync.json")).unwrap(),
        a_marker
    );
    let b_files = files(&b.0);

    let running = match start(&data, &a.0) {
        Ok(running) => running,
        Err(error) => panic!("A was refused: {error}"),
    };
    running.cli("rescan", &["--timeout", "60s"], true);
    let objects = running.objects();
    let conflicts = running.cli("conflicts", &[], true);
    let status = running.cli("status", &[], true);
    let events = running.stop();
    let by_name = |name: &str| {
        objects
            .iter()
            .find(|(object, ..)| object == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} is gone: {objects:?} {events:?}"))
    };
    let kept = |name: &str| {
        changed
            .iter()
            .find(|(object, ..)| object == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_name("x"), kept("x"), "{objects:?}");
    assert_eq!(by_name("new"), kept("new"), "{objects:?}");
    assert_eq!(by_name("z"), kept("z"), "{objects:?}");
    assert_eq!(by_name("y").2, b"v1");
    assert_eq!(objects.len(), 4, "{objects:?}");
    let retained = conflicts["items"].as_array().unwrap();
    assert_eq!(retained.len(), 1, "{conflicts}");
    assert_eq!(retained[0]["object"], "x", "{conflicts}");
    assert_eq!(status["pending_deletion_count"], 0, "{status}");
    assert_eq!(status["root_path"], canonical(&a.0), "{status}");
    assert_eq!(
        status["root_change"],
        serde_json::json!({"reason": "marked_directory", "previous_paths": [canonical(&b.0)]})
    );
    nothing_deleted_logged(&events);
    switched(&events, "marked_directory", &[b.0.as_path()], &a.0);
    assert_eq!(
        fs::read(a.0.join(".gcs-mimic-sync.json")).unwrap(),
        a_marker
    );
    assert_eq!(fs::read(a.0.join("test-bucket/new")).unwrap(), b"new");
    assert_eq!(files(&b.0), b_files);

    let running = start(&data, &c.0).unwrap();
    let events = running.stop();
    let change = switched(&events, "empty_directory", &[a.0.as_path()], &c.0);
    assert_eq!(change["forgotten_retained_conflicts"], 1, "{change}");
    let forgotten = &change["forgotten_conflicts"][0];
    assert_eq!(forgotten["object"], "x", "{change}");
    let path = forgotten["path"].as_str().unwrap();
    assert!(data.0.join(path).is_file(), "{change}");
}

/// No deletion was applied, whichever the tree.
fn nothing_deleted_logged(events: &[Value]) {
    assert!(
        events
            .iter()
            .all(|event| event["event"] != "sync_deletions_applied"),
        "{events:?}"
    );
}

/// A directory without a marker that holds files below it is refused, naming the synchronized
/// directory, and none of its files becomes an object. The synchronized directory then opens as
/// before, with its objects and generations as they were.
#[test]
fn a_directory_with_files_and_no_marker_is_refused_and_nothing_is_imported() {
    let data = TestDir::new();
    let (a, b) = (TestDir::new(), TestDir::new());
    let first = synchronized(&data, &a.0);
    fs::create_dir(b.0.join("test-bucket")).unwrap();
    fs::write(b.0.join("test-bucket/notes.txt"), b"mine").unwrap();

    let error = match start(&data, &b.0) {
        Ok(running) => {
            running.cli("rescan", &["--timeout", "60s"], true);
            let objects = running.objects();
            running.stop();
            panic!("B was accepted: {objects:?}");
        }
        Err(error) => error,
    };
    assert!(error.contains("ReplacementOccupied"), "{error}");
    assert!(error.contains(&canonical(&a.0)), "{error}");
    assert_eq!(files(&b.0), [("notes.txt".to_owned(), b"mine".to_vec())]);
    assert!(!b.0.join(".gcs-mimic-sync.json").exists());
    assert!(!b.0.join(".gcs-mimic-sync.json.staging").exists());

    let running = start(&data, &a.0).unwrap();
    running.settle();
    assert_eq!(running.objects(), first);
    assert!(running.cli("status", &[], true)["root_change"].is_null());
    let events = running.stop();
    nothing_deleted_logged(&events);
    assert!(
        events
            .iter()
            .all(|event| event["event"] != "sync_root_switched"),
        "{events:?}"
    );
}

/// A synchronized directory moved to another path keeps its marker. Startup merges it at the new
/// path under the same root, logs both paths, and changes no object.
#[test]
fn a_moved_root_is_merged_at_its_new_path() {
    let data = TestDir::new();
    let (a, aside) = (TestDir::new(), TestDir::new());
    let first = synchronized(&data, &a.0);
    let before = canonical(&a.0);
    let root = marker_root(&a.0);
    let moved = aside.0.join("moved");
    fs::rename(&a.0, &moved).unwrap();

    let running = match start(&data, &moved) {
        Ok(running) => running,
        Err(error) => panic!("the moved root was refused: {error}"),
    };
    running.settle();
    assert_eq!(running.objects(), first);
    let status = running.cli("status", &[], true);
    assert_eq!(status["pending_deletion_count"], 0, "{status}");
    assert_eq!(status["root_change"]["previous_paths"][0], before.as_str());
    let events = running.stop();
    nothing_deleted_logged(&events);
    let change = events
        .iter()
        .find(|event| event["event"] == "sync_root_switched")
        .unwrap_or_else(|| panic!("no change logged: {events:?}"));
    assert_eq!(change["reason"], "marked_directory", "{change}");
    assert_eq!(change["previous_roots"][0]["path"], before.as_str());
    assert_eq!(change["path"], canonical(&moved), "{change}");
    assert_eq!(marker_root(&moved), root);
    assert_eq!(change["root_id"], root, "{change}");
    fs::rename(&moved, &a.0).unwrap();
}
