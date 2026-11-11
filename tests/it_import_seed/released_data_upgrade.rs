//! Data directories written by published releases must open with this build and
//! serve the same resources. The fixtures come from the release images through
//! `scripts/released-data-fixtures.sh`; a hand-downgraded current catalog could not
//! show that a real user's directory upgrades.

use crate::common;

use common::TestDir;
use common::process::declare_server;
use md5::{Digest, Md5};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::Stdio,
    time::Duration,
};

/// Every stable release and candidate whose data directory the contract promises to upgrade.
const SUPPORTED_RELEASES: &[&str] = &[
    "0.8.0", "0.9.0", "0.10.0", "0.11.0", "0.12.0", "0.13.0", "0.14.0", "0.15.0", "0.16.0",
    "0.17.0", "0.17.1", "0.18.0",
];

/// Link fields embed the advertised base URL of whichever server answered.
const ADDRESS_FIELDS: &[&str] = &["selfLink", "mediaLink"];

impl Server {
    fn start(data: &Path) -> Self {
        Self::launch(
            common::server_command()
                .env("GCS_MIMIC_LISTEN", "127.0.0.1:0")
                .args(["serve", "--scheme", "http", "--data-dir"])
                .arg(data)
                .stderr(Stdio::null()),
        )
    }

    /// HTTP/1.0 keeps the body unchunked and closes the connection after it.
    fn get(&self, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("response head");
        let head = String::from_utf8_lossy(&raw[..split]);
        assert!(head.starts_with("HTTP/1.0 200 "), "GET {path}: {head}");
        raw.split_off(split + 4)
    }

    fn json(&self, path: &str) -> Value {
        serde_json::from_slice(&self.get(path)).unwrap()
    }

    fn stop(mut self) {
        self.0.stop();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The current build may add fields, but every field the release returned keeps its value.
fn assert_preserved(release: &str, expected: &Value, actual: &Value) {
    let expected = expected.as_object().unwrap();
    for (key, value) in expected {
        if ADDRESS_FIELDS.contains(&key.as_str()) {
            continue;
        }
        assert_eq!(
            actual.get(key),
            Some(value),
            "{release}: field {key} of {}",
            expected["id"]
        );
    }
}

fn items<'a>(listing: &'a Value, release: &str) -> &'a [Value] {
    listing["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{release}: listing without items: {listing}"))
}

fn encode_component(name: &str) -> String {
    name.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let triple = chunk.iter().enumerate().fold(0u32, |acc, (index, &byte)| {
            acc | u32::from(byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(triple >> (18 - 6 * index) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn check_release(release: &str) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/released-data")
        .join(release);
    let expected: Value =
        serde_json::from_slice(&fs::read(fixture.join("expected.json")).unwrap()).unwrap();
    // The server upgrades in place, so work on a copy and keep the fixture unchanged.
    let dir = TestDir::new();
    let data = dir.0.join("data");
    copy_tree(&fixture.join("data"), &data);

    // A second start proves the upgraded directory reopens, not just that it migrated once.
    for _ in 0..2 {
        let server = Server::start(&data);
        let buckets = server.json("/storage/v1/b?project=fixture");
        let expected_buckets = items(&expected["buckets"], release);
        assert_eq!(items(&buckets, release).len(), expected_buckets.len());
        for bucket in expected_buckets {
            let name = bucket["name"].as_str().unwrap();
            assert_preserved(
                release,
                bucket,
                &server.json(&format!("/storage/v1/b/{name}")),
            );
        }
        for (bucket, listing) in [
            ("released", "/storage/v1/b/released/o?versions=true"),
            ("plain", "/storage/v1/b/plain/o"),
        ] {
            let expected_objects = items(&expected[bucket], release);
            let actual = server.json(listing);
            assert_eq!(items(&actual, release).len(), expected_objects.len());
            for object in expected_objects {
                let name = encode_component(object["name"].as_str().unwrap());
                let generation = object["generation"].as_str().unwrap();
                let path = format!("/storage/v1/b/{bucket}/o/{name}?generation={generation}");
                let actual = server.json(&path);
                assert_preserved(release, object, &actual);
                assert_eq!(
                    actual["timeStorageClassUpdated"], actual["timeCreated"],
                    "{release}: pre-transition class time"
                );
                let body = server.get(&format!("{path}&alt=media"));
                assert_eq!(
                    base64(&Md5::digest(&body)),
                    object["md5Hash"].as_str().unwrap(),
                    "{release}: bytes of {path}"
                );
            }
        }
        server.stop();
    }
}

#[test]
fn supported_release_data_directories_upgrade_without_changing_resources() {
    for release in SUPPORTED_RELEASES {
        check_release(release);
    }
}

#[test]
fn every_supported_release_has_a_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/released-data");
    let mut present: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    present.sort();
    // Directory names sort lexically ("0.10.0" before "0.8.0"), so sort both sides
    // the same way; only the set of releases matters.
    let mut supported = SUPPORTED_RELEASES.to_vec();
    supported.sort_unstable();
    assert_eq!(present, supported);
}

declare_server!();
