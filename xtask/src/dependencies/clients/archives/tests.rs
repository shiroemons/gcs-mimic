use super::*;
use crate::dependencies::exceptions::Report;
use serde_json::json;

const ARM: &str =
    "https://github.com/Kitware/CMake/releases/download/v4.5.0/cmake-4.5.0-linux-aarch64.tar.gz";
const X86: &str =
    "https://github.com/Kitware/CMake/releases/download/v4.5.0/cmake-4.5.0-linux-x86_64.tar.gz";
const SDK: &str = "https://github.com/googleapis/google-cloud-cpp/archive/refs/tags/v3.13.0.tar.gz";

fn hex(digit: char) -> String {
    digit.to_string().repeat(64)
}

/// A Dockerfile in the shape of the probe's: a commented-out `ADD`, an `ADD` split over a
/// continuation line, and a `COPY`, which pins nothing.
fn dockerfile() -> String {
    format!(
        "# syntax=docker/dockerfile:1@sha256:{frontend}\n\
         FROM gcc:16.2.0-trixie@sha256:{image}\n\
         # ADD https://example.invalid/unpinned.tar.gz /dl/\n\
         ADD --checksum=sha256:{a} {ARM} /dl/cmake-arm64.tar.gz\n\
         ADD --checksum=sha256:{b} {X86} /dl/cmake-amd64.tar.gz\n\
         RUN tar -xzf \"/dl/cmake-${{TARGETARCH}}.tar.gz\" -C /opt/cmake\n\
         add --checksum=sha256:{c} \\\n    {SDK} /dl/google-cloud-cpp.tar.gz\n\
         COPY probe.cc /src/\n",
        frontend = hex('f'),
        image = hex('e'),
        a = hex('a'),
        b = hex('b'),
        c = hex('c'),
    )
}

fn scope() -> Value {
    json!({
        "manifest": "sdk-scorecard/cpp/Dockerfile",
        "toolchain": {"image": "gcc:16.2.0-trixie", "version": "16.2.0"},
        "direct_dependencies": [
            {"name": "cmake", "current": "v4.5.0", "repository": "Kitware/CMake", "license": "BSD-3-Clause",
             "archives": [{"url": ARM, "sha256": hex('a')}, {"url": X86, "sha256": hex('b')}]},
            {"name": "google-cloud-cpp", "current": "v3.13.0", "repository": "googleapis/google-cloud-cpp", "license": "Apache-2.0",
             "archives": [{"url": SDK, "sha256": hex('c')}]}
        ]
    })
}

/// The offline comparison of a Dockerfile with a snapshot scope.
fn offline(dockerfile: &str, scope: &Value) -> std::result::Result<(), ArchiveError> {
    let dependencies = dependencies(scope).unwrap();
    verify(&pins(dockerfile)?, &dependencies)
}

#[test]
fn pins_are_read_from_every_add_including_continuations_and_lowercase() {
    let pins = pins(&dockerfile()).unwrap();
    let read: Vec<(&str, &str, &str)> = pins
        .iter()
        .map(|pin| {
            (
                pin.release.repository.as_str(),
                pin.release.tag.as_str(),
                pin.url.as_str(),
            )
        })
        .collect();
    assert_eq!(
        read,
        [
            ("Kitware/CMake", "v4.5.0", ARM),
            ("Kitware/CMake", "v4.5.0", X86),
            ("googleapis/google-cloud-cpp", "v3.13.0", SDK),
        ]
    );
    assert_eq!(pins[2].sha256, hex('c'));
}

#[test]
fn a_matching_dockerfile_and_snapshot_verify() {
    assert_eq!(offline(&dockerfile(), &scope()), Ok(()));
    verify_toolchain(&dockerfile(), &scope()["toolchain"]).unwrap();
}

#[test]
fn a_changed_checksum_is_a_mismatch() {
    let changed = dockerfile().replace(&hex('b'), &hex('d'));
    assert_eq!(
        offline(&changed, &scope()),
        Err(ArchiveError::ChecksumMismatch {
            url: X86.to_owned(),
            dockerfile: hex('d'),
            snapshot: hex('b'),
        })
    );
}

#[test]
fn another_release_tag_is_a_version_mismatch() {
    let changed = dockerfile().replace("tags/v3.13.0.tar.gz", "tags/v3.12.0.tar.gz");
    assert_eq!(
        offline(&changed, &scope()),
        Err(ArchiveError::VersionMismatch {
            repository: "googleapis/google-cloud-cpp".to_owned(),
            dockerfile: "v3.12.0".to_owned(),
            snapshot: "v3.13.0".to_owned(),
        })
    );
}

#[test]
fn another_url_of_the_recorded_release_is_a_mismatch() {
    let changed = dockerfile().replace("linux-x86_64", "linux-riscv64");
    let error = offline(&changed, &scope()).unwrap_err();
    assert_eq!(
        error,
        ArchiveError::UrlMismatch {
            url: X86.replace("linux-x86_64", "linux-riscv64"),
            recorded: vec![ARM.to_owned(), X86.to_owned()],
        }
    );
    assert!(error.to_string().contains(ARM), "{error}");
}

#[test]
fn an_archive_missing_from_either_side_fails() {
    let without_x86: String = dockerfile()
        .lines()
        .filter(|line| !line.contains(X86))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(
        offline(&without_x86, &scope()),
        Err(ArchiveError::MissingFromDockerfile(X86.to_owned()))
    );
    let json_archive = "https://github.com/nlohmann/json/releases/download/v3.12.0/json.tar.xz";
    let with_json = format!(
        "{}ADD --checksum=sha256:{} {json_archive} /dl/json.tar.xz\n",
        dockerfile(),
        hex('9')
    );
    assert_eq!(
        offline(&with_json, &scope()),
        Err(ArchiveError::UnrecordedRelease(json_archive.to_owned()))
    );
}

#[test]
fn an_add_without_a_checksum_fails() {
    for line in [
        format!("ADD {SDK} /dl/google-cloud-cpp.tar.gz"),
        format!("ADD --chmod=644 {SDK} /dl/google-cloud-cpp.tar.gz"),
        "add probe.tar /src/".to_owned(),
    ] {
        let error = pins(&format!("{}{line}\n", dockerfile())).unwrap_err();
        assert!(
            matches!(error, ArchiveError::MissingChecksum(_)),
            "{line}: {error:?}"
        );
        assert!(error.to_string().contains("--checksum"), "{error}");
    }
}

#[test]
fn an_add_in_any_other_form_fails() {
    let unsupported = |line: &str| pins(&format!("{line}\n")).unwrap_err();
    for line in [
        format!("ADD --checksum=sha512:{} {SDK} /dl/x", hex('a')),
        format!("ADD --checksum=sha256:{} {SDK} /dl/x", "a".repeat(63)),
        format!("ADD --checksum=sha256:{} {SDK} /dl/x", "A".repeat(64)),
        format!("ADD --checksum=sha256:{} --chmod=644 {SDK} /dl/x", hex('a')),
        format!(
            "ADD --checksum=sha256:{} --checksum=sha256:{} {SDK} /dl/x",
            hex('a'),
            hex('a')
        ),
        format!("ADD --checksum=sha256:{} {SDK} {ARM} /dl/", hex('a')),
        format!("ADD --checksum=sha256:{} {SDK}", hex('a')),
    ] {
        assert!(
            matches!(unsupported(&line), ArchiveError::UnsupportedAdd(_)),
            "{line}"
        );
    }
    assert!(matches!(
        unsupported(&format!(
            "ADD --checksum=sha256:{} [\"{SDK}\", \"/dl/x\"]",
            hex('a')
        )),
        ArchiveError::UnsupportedUrl(_)
    ));
    let twice = format!("ADD --checksum=sha256:{} {SDK} /dl/y\n", hex('c'));
    assert_eq!(
        pins(&format!("{}{twice}", dockerfile())),
        Err(ArchiveError::DuplicateArchive(SDK.to_owned()))
    );
}

#[test]
fn archive_urls_name_a_github_release_or_tag_archive_only() {
    for (url, repository, tag) in [
        (ARM, "Kitware/CMake", "v4.5.0"),
        (SDK, "googleapis/google-cloud-cpp", "v3.13.0"),
        (
            "https://github.com/abseil/abseil-cpp/releases/download/20260817.0/abseil-cpp-20260817.0.tar.gz",
            "abseil/abseil-cpp",
            "20260817.0",
        ),
    ] {
        assert_eq!(
            url.parse::<Release>(),
            Ok(Release {
                repository: repository.to_owned(),
                tag: tag.to_owned()
            })
        );
    }
    for url in [
        SDK.replace("https://", "http://"),
        SDK.replace("github.com", "github.com.example"),
        SDK.replace("refs/tags/", "refs/heads/"),
        SDK.replace("refs/tags/", ""),
        SDK.replace(".tar.gz", ".zip"),
        format!("{SDK}?download=1"),
        SDK.replace("googleapis", ".."),
        ARM.replace("cmake-4.5.0-linux-aarch64.tar.gz", ""),
        ARM.replace("v4.5.0/", "v4.5.0/nested/"),
        "https://github.com/Kitware/CMake".to_owned(),
    ] {
        assert_eq!(
            url.parse::<Release>(),
            Err(ArchiveError::UnsupportedUrl(url.clone())),
            "{url}"
        );
    }
}

/// An edit that breaks the fixture snapshot, and the error it must cause.
type SnapshotCase = (fn(&mut Value), &'static str);

#[test]
fn snapshot_records_must_name_their_own_release() {
    let cases: [SnapshotCase; 7] = [
        (
            |s| s["direct_dependencies"][0]["current"] = json!("v4.6.0"),
            "is not from Kitware/CMake v4.6.0",
        ),
        (
            |s| s["direct_dependencies"][1]["repository"] = json!("googleapis/other"),
            "is not from googleapis/other v3.13.0",
        ),
        (
            |s| s["direct_dependencies"][1] = s["direct_dependencies"][0].clone(),
            "repeats Kitware/CMake",
        ),
        (
            |s| {
                s["direct_dependencies"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("license");
            },
            "missing string: license",
        ),
        (
            |s| s["direct_dependencies"][1]["archives"] = json!([]),
            "missing source archives: google-cloud-cpp",
        ),
        (
            |s| s["direct_dependencies"][1]["archives"][0]["sha256"] = Value::from("C".repeat(64)),
            "invalid SHA-256",
        ),
        (
            |s| s["direct_dependencies"][1]["current"] = json!("v3.14.0-rc1"),
            "release tag v3.14.0-rc1",
        ),
    ];
    for (edit, expected) in cases {
        let mut scope = scope();
        edit(&mut scope);
        let error = dependencies(&scope).err().unwrap_or_default();
        assert!(error.contains(expected), "{expected}: {error}");
    }
    assert!(dependencies(&json!({"direct_dependencies": []})).is_err());
}

#[test]
fn the_toolchain_is_the_digest_pinned_image_the_dockerfile_builds_from() {
    let toolchain = |image: &str, version: &str| json!({"image": image, "version": version});
    let platform = dockerfile().replace("FROM gcc", "FROM --platform=$BUILDPLATFORM gcc");
    assert!(verify_toolchain(&platform, &toolchain("gcc:16.2.0-trixie", "16.2.0")).is_ok());
    for (source, image, version) in [
        (dockerfile(), "gcc:16.2.0-trixie", "16.1.0"),
        (dockerfile(), "gcc:16.3.0-trixie", "16.3.0"),
        (
            dockerfile().replace("@sha256:", ":latest@sha256:"),
            "gcc:16.2.0-trixie",
            "16.2.0",
        ),
        (
            dockerfile().replace("FROM gcc", "# FROM gcc"),
            "gcc:16.2.0-trixie",
            "16.2.0",
        ),
        (
            dockerfile().replace(&format!("@sha256:{}", hex('e')), ""),
            "gcc:16.2.0-trixie",
            "16.2.0",
        ),
    ] {
        assert!(
            verify_toolchain(&source, &toolchain(image, version)).is_err(),
            "{image} {version}"
        );
    }
}

#[test]
fn a_newer_latest_release_is_an_update_and_an_older_one_an_error() {
    let mut report = Report::without_exceptions();
    let inputs = &mut report.inputs_of("sdk-scorecard/cpp/Dockerfile");
    assert_eq!(
        compare_release(inputs, "o/r", "v4.5.0", "v4.5.0"),
        Ok(false)
    );
    assert_eq!(compare_release(inputs, "o/r", "v4.5.0", "v4.5.1"), Ok(true));
    assert_eq!(
        compare_release(inputs, "o/r", "v3.13.0", "v4.0.0"),
        Ok(true)
    );
    assert_eq!(
        compare_release(inputs, "o/r", "20260817.0", "20270215.0"),
        Ok(true)
    );
    assert_eq!(
        compare_release(inputs, "o/r", "v4.5.0", "v4.4.6"),
        Err(ArchiveError::OlderLatest {
            repository: "o/r".to_owned(),
            current: "v4.5.0".to_owned(),
            latest: "v4.4.6".to_owned(),
        })
    );
    assert_eq!(
        compare_release(inputs, "o/r", "v4.5.0", "v4.6.0-rc1"),
        Err(ArchiveError::UnsupportedTag("v4.6.0-rc1".to_owned()))
    );
}

#[test]
fn only_a_published_stable_release_counts_as_latest() {
    let stable = json!({"tag_name": "v3.13.0", "draft": false, "prerelease": false});
    assert_eq!(latest_release_tag(&stable).unwrap(), "v3.13.0");
    for release in [
        json!({"tag_name": "v3.14.0-rc1", "draft": false, "prerelease": true}),
        json!({"tag_name": "v3.14.0", "draft": true, "prerelease": false}),
        json!({"tag_name": "v3.14.0"}),
        json!({"draft": false, "prerelease": false}),
    ] {
        assert!(latest_release_tag(&release).is_err(), "{release}");
    }
}

/// The committed probe Dockerfile and its snapshot entry agree, so `cargo test` catches a pin
/// changed on one side without the network.
#[test]
fn the_committed_cpp_probe_pins_match_the_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let snapshot = json(&read(root, "compatibility/dependency-status.json").unwrap()).unwrap();
    let scope = snapshot["sdk_ecosystems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["ecosystem"] == "source-archive")
        .unwrap();
    let manifest = text(scope, "manifest").unwrap();
    let dockerfile = read(root, manifest).unwrap();
    let dependencies = dependencies(scope).unwrap();
    let pins = pins(&dockerfile).unwrap();
    assert_eq!(pins.len(), 6);
    verify(&pins, &dependencies).unwrap();
    verify_toolchain(&dockerfile, &scope["toolchain"]).unwrap();
}
