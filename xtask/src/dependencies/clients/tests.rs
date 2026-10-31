use super::*;
use serde_json::json;

#[test]
fn stable_registry_versions_include_two_and_four_components() {
    assert_eq!(numeric_release("3.0"), Some([3, 0, 0, 0]));
    assert_eq!(numeric_release("3.0.0.1"), Some([3, 0, 0, 1]));
    assert_eq!(latest_maven("<versions><version>2.0.0</version><version>3.0</version><version>4.0-rc1</version></versions>").unwrap(), "3.0");
    for value in ["3.0-rc1", "3.0.0.1-beta", "3..0", "3.0.0.0.1"] {
        assert!(numeric_release(value).is_none());
    }
}

#[test]
fn java_hotfix_builds_use_the_full_release_name() {
    assert_eq!(
        temurin_version(
            &json!([{"release_name":"jdk-27.0.1.0.2+1","version":{"openjdk_version":"27.0.1.0"}}])
        )
        .unwrap(),
        "27.0.1.0.2+1"
    );
    assert!(temurin_version(&json!([{"release_name":"jdk-28-ea+1"}])).is_err());
}

#[test]
fn snapshots_reject_missing_duplicate_and_changed_dependencies() {
    let pins = BTreeMap::from([("x".to_owned(), "1.0.0".to_owned())]);
    assert!(
        snapshot_pins(
            &json!({"direct_dependencies":[{"name":"x","current":"1.0.0"}]}),
            &pins
        )
        .is_ok()
    );
    for records in [
        json!([]),
        json!([{"name":"x","current":"2.0.0"}]),
        json!([{"name":"x","current":"1.0.0"},{"name":"x","current":"1.0.0"}]),
    ] {
        assert!(snapshot_pins(&json!({"direct_dependencies":records}), &pins).is_err());
    }
}

#[test]
fn maven_reads_actual_dependency_and_plugin_pins_and_rejects_properties() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let source = read(root, "sdk-tests/java/pom.xml").unwrap();
    let (packages, plugins) = maven_pins(&source).unwrap();
    assert_eq!(packages["com.google.cloud:google-cloud-storage"], "2.76.0");
    assert_eq!(packages.len(), 2);
    assert_eq!(plugins.len(), 3);
    for changed in [
        source.replace("2.76.0", "${storage.version}"),
        source.replace("2.76.0", "2.76.0-rc1"),
        source.replace("</dependency>", ""),
        source.replace(
            "<groupId>com.google.cloud</groupId>",
            "<groupId>&group;</groupId>",
        ),
    ] {
        assert!(maven_pins(&changed).is_err());
    }
    assert_eq!(latest_maven("<versions><version>2.0.0</version><version>3.0.0-rc1</version><version>2.1.0</version></versions>").unwrap(), "2.1.0");
    assert!(latest_maven("<versions><version>3.0.0-rc1</version></versions>").is_err());
}

#[test]
fn nuget_reads_actual_exact_pins_and_rejects_ranges_duplicates_or_properties() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let source = read(root, "sdk-tests/dotnet/Scenario.csproj").unwrap();
    let pins = nuget_pins(&source).unwrap();
    assert_eq!(pins["Google.Cloud.Storage.V1"], "5.0.0");
    assert_eq!(pins.len(), 2);
    for changed in [
        source.replace("[5.0.0]", "5.0.0"),
        source.replace("[5.0.0]", "[5.0.0,6.0.0)"),
        source.replace("[5.0.0]", "[$(StorageVersion)]"),
        source.replace(
            "Include=\"Testcontainers\"",
            "Include=\"Google.Cloud.Storage.V1\"",
        ),
    ] {
        assert!(nuget_pins(&changed).is_err());
    }
}

#[test]
fn nuget_pages_exclude_unlisted_and_prerelease_and_fail_closed() {
    let base = "https://api.nuget.org/v3/registration5-gz-semver2/";
    let source = json!({"items":[{"@id":format!("{base}x/page.json")}]});
    let page = json!({"items":[
        {"catalogEntry":{"version":"1.0.0"}},
        {"catalogEntry":{"version":"2.0.0","listed":true}},
        {"catalogEntry":{"version":"3.0.0","listed":false}},
        {"catalogEntry":{"version":"4.0.0-rc1","listed":true}}
    ]});
    let mut urls = Vec::new();
    assert_eq!(
        latest_nuget(&source, base, &mut |url| {
            urls.push(url.to_owned());
            Ok(page.to_string())
        })
        .unwrap(),
        "2.0.0"
    );
    assert_eq!(urls.len(), 1);
    assert!(latest_nuget(&source, base, &mut |_| Err("network failure".into())).is_err());
    assert!(
        latest_nuget(
            &json!({"items":[{"@id":"https://other.invalid/page"}]}),
            base,
            &mut |_| panic!("must not fetch another host")
        )
        .is_err()
    );
    assert!(
        latest_nuget(
            &json!({"items":[{"items":[{"catalogEntry":{"version":"1.0.0","listed":false}}]}]}),
            base,
            &mut |_| unreachable!()
        )
        .is_err()
    );
}

#[test]
fn every_extra_manifest_is_declared_in_the_canonical_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let snapshot = json(&read(root, "compatibility/dependency-status.json").unwrap()).unwrap();
    for manifest in [
        "go/go.mod",
        "go/sourceexport/go.mod",
        "sdk-tests/rust/Cargo.toml",
        "sdk-tests/java/pom.xml",
        "sdk-tests/dotnet/Scenario.csproj",
        "sdk-tests/testcontainers/node/package.json",
    ] {
        let scopes = snapshot["sdk_ecosystems"].as_array().unwrap();
        assert_eq!(
            scopes
                .iter()
                .filter(|scope| scope["manifest"] == manifest)
                .count(),
            1,
            "{manifest}"
        );
    }
}

#[test]
fn npm_manifest_and_integrity_lock_must_agree() {
    let pins = BTreeMap::from([("x".to_owned(), "1.0.0".to_owned())]);
    let mut lock = json!({"packages":{"":{"dependencies":{"x":"1.0.0"}},"node_modules/x":{"version":"1.0.0","integrity":"sha512-locked"}}});
    assert!(super::super::npm_lock(&lock, &pins).is_ok());
    lock["packages"]["node_modules/x"]["version"] = json!("2.0.0");
    assert!(super::super::npm_lock(&lock, &pins).is_err());
    lock["packages"]["node_modules/x"]["version"] = json!("1.0.0");
    lock["packages"]["node_modules/x"]
        .as_object_mut()
        .unwrap()
        .remove("integrity");
    assert!(super::super::npm_lock(&lock, &pins).is_err());
}
