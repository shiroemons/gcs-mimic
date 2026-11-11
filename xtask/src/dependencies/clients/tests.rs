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
    assert_eq!(packages["com.google.cloud:google-cloud-storage"], "2.78.0");
    assert_eq!(packages.len(), 2);
    assert_eq!(plugins.len(), 3);
    for changed in [
        source.replace("2.78.0", "${storage.version}"),
        source.replace("2.78.0", "2.78.0-rc1"),
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
fn nuget_target_frameworks_are_one_element_or_a_list_of_distinct_runtimes() {
    let project = |body: &str| format!("<Project><PropertyGroup>{body}</PropertyGroup></Project>");
    assert_eq!(
        target_frameworks(&project("<TargetFramework>net11.0</TargetFramework>")).unwrap(),
        ["net11.0"]
    );
    assert_eq!(
        target_frameworks(&project(
            "<TargetFrameworks>net10.0;net11.0</TargetFrameworks>"
        ))
        .unwrap(),
        ["net10.0", "net11.0"]
    );
    for body in [
        "",
        "<TargetFramework>net11.0</TargetFramework><TargetFrameworks>net10.0;net11.0</TargetFrameworks>",
        "<TargetFrameworks>net10.0;net10.0</TargetFrameworks>",
        "<TargetFrameworks>net10.0;</TargetFrameworks>",
        "<TargetFramework>netstandard2.0</TargetFramework>",
        "<TargetFramework>net11.0-windows</TargetFramework>",
    ] {
        assert!(target_frameworks(&project(body)).is_err(), "{body}");
    }
}

#[test]
fn every_dotnet_target_runs_on_a_pinned_runtime() {
    let frameworks = |list: &[&str]| list.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
    let both = frameworks(&["net10.0", "net11.0"]);
    assert!(runtime_coverage(&frameworks(&["net11.0"]), "11.0.100", None).is_ok());
    assert!(runtime_coverage(&both, "11.0.100", Some("10.0.403")).is_ok());
    // A net10.0 target without the LTS SDK pin has no runtime in CI.
    assert!(runtime_coverage(&both, "11.0.100", None).is_err());
    // An LTS pin that no target uses is stale.
    assert!(runtime_coverage(&frameworks(&["net11.0"]), "11.0.100", Some("10.0.403")).is_err());
    assert!(
        runtime_coverage(
            &frameworks(&["net9.0", "net11.0"]),
            "11.0.100",
            Some("10.0.403")
        )
        .is_err()
    );
    assert!(runtime_coverage(&both, "11.0", Some("10.0.403")).is_err());
}

#[test]
fn nuget_lock_must_hold_exactly_the_project_frameworks() {
    let pins = BTreeMap::from([("x".to_owned(), "1.0.0".to_owned())]);
    let entry = json!({"type":"Direct","requested":"[1.0.0, 1.0.0]","resolved":"1.0.0","contentHash":"hash"});
    let both = ["net10.0".to_owned(), "net11.0".to_owned()];
    let mut lock = json!({"dependencies":{"net10.0":{"x":entry},"net11.0":{"x":entry}}});
    assert!(locked_direct_pins(&lock, &both, &pins).is_ok());
    assert!(locked_direct_pins(&lock, &both[1..], &pins).is_err());
    lock["dependencies"]["net10.0"]["x"]["resolved"] = json!("2.0.0");
    assert!(locked_direct_pins(&lock, &both, &pins).is_err());
    let one = json!({"dependencies":{"net11.0":{"x":entry}}});
    assert!(locked_direct_pins(&one, &both, &pins).is_err());
}

#[test]
fn dotnet_sdks_compare_with_the_newest_channel_and_the_pinned_lts_channel() {
    let releases = json!({"releases-index":[
        {"channel-version":"11.0","latest-sdk":"11.0.100","release-type":"sts","support-phase":"active"},
        {"channel-version":"10.0","latest-sdk":"10.0.404","release-type":"lts","support-phase":"active"},
        {"channel-version":"9.0","latest-sdk":"9.0.312","release-type":"sts","support-phase":"active"},
        {"channel-version":"8.0","latest-sdk":"8.0.420","release-type":"lts","support-phase":"eol"},
        {"channel-version":"12.0","latest-sdk":"12.0.100-preview.1","release-type":"lts","support-phase":"preview"}
    ]});
    assert_eq!(newest_sdk(&releases).unwrap(), "11.0.100");
    assert_eq!(lts_channel_sdk(&releases, "10.0.403").unwrap(), "10.0.404");
    for pin in ["9.0.300", "8.0.400", "7.0.100", "10.0"] {
        assert!(lts_channel_sdk(&releases, pin).is_err(), "{pin}");
    }
}

#[test]
fn the_dotnet_suite_targets_the_lts_and_sdk_runtimes_it_pins() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let frameworks =
        target_frameworks(&read(root, "sdk-tests/dotnet/Scenario.csproj").unwrap()).unwrap();
    assert_eq!(frameworks, ["net10.0", "net11.0"]);
    let global = json(&read(root, "sdk-tests/dotnet/global.json").unwrap()).unwrap();
    let lts = read(root, "sdk-tests/dotnet/.dotnet-lts-sdk-version").unwrap();
    assert!(
        runtime_coverage(
            &frameworks,
            global["sdk"]["version"].as_str().unwrap(),
            Some(lts.trim_end())
        )
        .is_ok()
    );
    let probe = read(root, "sdk-scorecard/dotnet/Probe.csproj").unwrap();
    assert_eq!(target_frameworks(&probe).unwrap(), ["net11.0"]);
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
        "sdk-scorecard/go/go.mod",
        "sdk-tests/rust/Cargo.toml",
        "sdk-tests/java/pom.xml",
        "sdk-tests/dotnet/Scenario.csproj",
        "sdk-tests/testcontainers/node/package.json",
        "sdk-scorecard/rust/Cargo.toml",
        "sdk-scorecard/java/pom.xml",
        "sdk-scorecard/dotnet/Probe.csproj",
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
