//! Additional client graphs stay outside the application dependency graph.

use super::{
    Result, compare, direct_crate_resolutions, fetch, index_path, json, latest_crate, text, version,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

pub(super) fn snapshot_pins(scope: &Value, pins: &BTreeMap<String, String>) -> Result<()> {
    let records = scope["direct_dependencies"]
        .as_array()
        .ok_or("missing SDK direct snapshot")?;
    if records.len() != pins.len() {
        return Err("SDK direct snapshot count mismatch".into());
    }
    for (name, pin) in pins {
        if records
            .iter()
            .filter(|record| record["name"] == *name && record["current"] == *pin)
            .count()
            != 1
        {
            return Err(format!("SDK direct snapshot mismatch: {name}"));
        }
    }
    Ok(())
}

/// Standalone client graphs: the official SDK suites and their scorecard probes keep separate
/// manifests and lockfiles, so each one is checked even when the pins agree.
pub(super) const MANIFESTS: &[(&str, &str)] = &[
    ("cargo", "sdk-tests/rust/Cargo.toml"),
    ("cargo", "sdk-scorecard/rust/Cargo.toml"),
    ("maven", "sdk-tests/java/pom.xml"),
    ("maven", "sdk-scorecard/java/pom.xml"),
    ("nuget", "sdk-tests/dotnet/Scenario.csproj"),
    ("nuget", "sdk-scorecard/dotnet/Probe.csproj"),
];

pub(super) fn check(root: &Path, scope: &Value) -> Result<bool> {
    let declared = (text(scope, "ecosystem")?, text(scope, "manifest")?);
    if !MANIFESTS.contains(&declared) {
        return Err("unsupported additional SDK manifest".into());
    }
    match declared.0 {
        "cargo" => cargo(root, scope),
        "maven" => maven(root, scope),
        _ => nuget(root, scope),
    }
}

fn read(root: &Path, path: &str) -> Result<String> {
    std::fs::read_to_string(root.join(path)).map_err(|error| error.to_string())
}

/// Reads a lock or runtime pin that lives beside the scope's manifest.
fn read_sibling(root: &Path, scope: &Value, name: &str) -> Result<String> {
    optional_sibling(root, scope, name)?
        .ok_or_else(|| format!("missing {name} beside the SDK manifest"))
}

/// Reads a pin beside the scope's manifest that only some manifests have.
fn optional_sibling(root: &Path, scope: &Value, name: &str) -> Result<Option<String>> {
    let manifest = Path::new(text(scope, "manifest")?);
    let dir = manifest.parent().ok_or("SDK manifest has no directory")?;
    match std::fs::read_to_string(root.join(dir).join(name)) {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn cargo(root: &Path, scope: &Value) -> Result<bool> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version=1",
            "--manifest-path",
            text(scope, "manifest")?,
        ])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Rust SDK locked metadata: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata = json(&String::from_utf8(output.stdout).map_err(|e| e.to_string())?)?;
    let mut pins = BTreeMap::new();
    for (name, resolutions) in direct_crate_resolutions(&metadata)? {
        if resolutions.len() != 1 {
            return Err(format!("ambiguous Rust SDK direct dependency: {name}"));
        }
        pins.insert(
            name,
            resolutions
                .first()
                .ok_or("empty Rust SDK resolution")?
                .clone(),
        );
    }
    snapshot_pins(scope, &pins)?;
    let toolchain = super::scalar(&read(root, "rust-toolchain.toml")?, "channel")?;
    let package = metadata["packages"]
        .as_array()
        .ok_or("missing SDK Cargo packages")?
        .iter()
        .find(|package| package["id"] == metadata["resolve"]["root"])
        .ok_or("missing Rust SDK package")?;
    if package["dependencies"]
        .as_array()
        .ok_or("missing Rust SDK dependency declarations")?
        .iter()
        .any(|dependency| {
            dependency["source"] != "registry+https://github.com/rust-lang/crates.io-index"
        })
    {
        return Err("Rust SDK dependencies must use the checked crates.io registry".into());
    }
    if package["rust_version"] != toolchain || scope["toolchain"]["version"] != toolchain {
        return Err("Rust SDK toolchain snapshot mismatch".into());
    }
    let mut drift = false;
    for (name, pin) in pins {
        let latest = latest_crate(&fetch(
            &format!("https://index.crates.io/{}", index_path(&name)),
            &[],
            false,
        )?)?;
        drift |= compare(&format!("cargo:{name} (SDK)"), &pin, &latest);
    }
    Ok(drift)
}

// These checked-in manifests deliberately use literal tags and exact pins. Reject
// entities/properties instead of guessing at arbitrary Maven or MSBuild evaluation.
fn elements<'a>(source: &'a str, tag: &str) -> Result<Vec<&'a str>> {
    if source.contains("<!") || source.contains('&') || source.contains("${") {
        return Err("SDK XML must contain literal elements without entities or properties".into());
    }
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut result = Vec::new();
    let mut rest = source;
    while let Some((_, body)) = rest.split_once(&open) {
        let (value, tail) = body.split_once(&close).ok_or("unclosed SDK XML element")?;
        result.push(value);
        rest = tail;
    }
    if source.matches(&close).count() != result.len() {
        return Err("unmatched SDK XML element".into());
    }
    Ok(result)
}

fn element(source: &str, tag: &str) -> Result<String> {
    let values = elements(source, tag)?;
    let [value] = values.as_slice() else {
        return Err(format!("expected one SDK XML {tag}"));
    };
    if value.is_empty() || value.contains(['<', '>']) {
        return Err("invalid SDK XML literal".into());
    }
    Ok((*value).to_owned())
}

fn maven_pins(source: &str) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    let mut packages = BTreeMap::new();
    let mut plugins = BTreeMap::new();
    for (tag, target) in [("dependency", &mut packages), ("plugin", &mut plugins)] {
        for entry in elements(source, tag)? {
            let group = element(entry, "groupId")?;
            let artifact = element(entry, "artifactId")?;
            let pin = element(entry, "version")?;
            if version(&pin).is_none()
                || !group
                    .bytes()
                    .chain(artifact.bytes())
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || target.insert(format!("{group}:{artifact}"), pin).is_some()
            {
                return Err("Maven dependencies/plugins require unique literal stable pins".into());
            }
        }
    }
    if packages.is_empty() || plugins.is_empty() {
        return Err("missing Maven dependency/plugin pins".into());
    }
    Ok((packages, plugins))
}

fn latest_maven(source: &str) -> Result<String> {
    elements(source, "version")?
        .into_iter()
        .filter_map(|pin| Some((numeric_release(pin)?, pin)))
        .max_by_key(|(numeric, _)| *numeric)
        .map(|(_, pin)| pin.to_owned())
        .ok_or("no stable Maven release".into())
}

fn numeric_release(pin: &str) -> Option<[u64; 4]> {
    let pieces: Vec<_> = pin.split('.').collect();
    if !(2..=4).contains(&pieces.len()) {
        return None;
    }
    let mut result = [0; 4];
    for (place, piece) in result.iter_mut().zip(pieces) {
        if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *place = piece.parse().ok()?;
    }
    Some(result)
}

fn maven(root: &Path, scope: &Value) -> Result<bool> {
    let (packages, plugins) = maven_pins(&read(root, text(scope, "manifest")?)?)?;
    snapshot_pins(scope, &packages)?;
    snapshot_pins(
        &serde_json::json!({"direct_dependencies":scope["plugins"]}),
        &plugins,
    )?;
    let lock = read_sibling(root, scope, "dependencies.lock")?;
    for (name, pin) in &packages {
        if !lock.contains(&format!("{name}:jar:{pin}:compile")) {
            return Err(format!("Maven dependency lock mismatch: {name}"));
        }
    }
    let mut drift = false;
    for (name, pin) in packages.into_iter().chain(plugins) {
        let (group, artifact) = name.split_once(':').ok_or("invalid Maven coordinate")?;
        let latest = latest_maven(&fetch(
            &format!(
                "https://repo.maven.apache.org/maven2/{}/{artifact}/maven-metadata.xml",
                group.replace('.', "/")
            ),
            &[],
            false,
        )?)?;
        drift |= compare(&format!("maven:{name}"), &pin, &latest);
    }
    let maven = read_sibling(root, scope, ".maven-version")?;
    if scope["maven_version"] != maven.trim() {
        return Err("Maven runtime snapshot mismatch".into());
    }
    drift |= compare(
        "Maven toolchain",
        maven.trim(),
        &latest_maven(&fetch(
            "https://repo.maven.apache.org/maven2/org/apache/maven/apache-maven/maven-metadata.xml",
            &[],
            false,
        )?)?,
    );
    let java = read_sibling(root, scope, ".java-version")?;
    if scope["toolchain"]["version"] != java.trim() {
        return Err("Java runtime snapshot mismatch".into());
    }
    let available = json(&fetch(
        "https://api.adoptium.net/v3/info/available_releases",
        &[],
        false,
    )?)?;
    let major = available["most_recent_feature_release"]
        .as_u64()
        .ok_or("missing Java GA feature release")?;
    let releases = json(&fetch(
        &format!(
            "https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jdk&os=linux&vendor=eclipse"
        ),
        &[],
        false,
    )?)?;
    let latest = temurin_version(&releases)?;
    drift |= compare("Temurin toolchain", java.trim(), latest);
    Ok(drift)
}

fn temurin_version(releases: &Value) -> Result<&str> {
    let name = releases
        .as_array()
        .and_then(|items| items.first())
        .and_then(|release| release["release_name"].as_str())
        .and_then(|name| name.strip_prefix("jdk-"))
        .ok_or("missing Java GA release name")?;
    if name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_digit() || b".+".contains(&b))
    {
        return Err("invalid Java GA release name".into());
    }
    Ok(name)
}

fn nuget_pins(source: &str) -> Result<BTreeMap<String, String>> {
    let mut pins = BTreeMap::new();
    for segment in source.split("<PackageReference").skip(1) {
        let (attributes, _) = segment.split_once("/>").ok_or("unclosed NuGet reference")?;
        let words: Vec<_> = attributes.split_whitespace().collect();
        let [name, pin] = words.as_slice() else {
            return Err("NuGet references require literal Include and Version only".into());
        };
        let name = name
            .strip_prefix("Include=\"")
            .and_then(|s| s.strip_suffix('"'))
            .ok_or("invalid NuGet package name")?;
        let pin = pin
            .strip_prefix("Version=\"[")
            .and_then(|s| s.strip_suffix("]\""))
            .ok_or("NuGet pin must be exact")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || version(pin).is_none()
            || pins.insert(name.to_owned(), pin.to_owned()).is_some()
        {
            return Err("invalid or duplicate NuGet dependency".into());
        }
    }
    if pins.is_empty() {
        return Err("missing NuGet dependencies".into());
    }
    Ok(pins)
}

fn latest_nuget(
    source: &Value,
    base: &str,
    get: &mut impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    let pages = source["items"]
        .as_array()
        .ok_or("missing NuGet registration pages")?;
    let mut latest = None;
    for page in pages {
        let owned;
        let page = if page["items"].is_array() {
            page
        } else {
            let url = text(page, "@id")?;
            if !url.starts_with(base) {
                return Err("unexpected NuGet registration page origin".into());
            }
            owned = json(&get(url)?)?;
            &owned
        };
        for leaf in page["items"]
            .as_array()
            .ok_or("missing NuGet page entries")?
        {
            let entry = &leaf["catalogEntry"];
            if !entry.is_object() {
                return Err("missing inline NuGet catalog entry".into());
            }
            if entry.get("listed").is_some_and(|value| !value.is_boolean()) {
                return Err("invalid NuGet listed marker".into());
            }
            if entry["listed"] == false {
                continue;
            }
            let pin = text(entry, "version")?;
            if let Some(numeric) = numeric_release(pin)
                && latest
                    .as_ref()
                    .is_none_or(|(previous, _)| numeric > *previous)
            {
                latest = Some((numeric, pin.to_owned()));
            }
        }
    }
    latest
        .map(|(_, pin)| pin)
        .ok_or("no listed stable NuGet release".into())
}

/// The exact SDK of an LTS channel, beside a .NET manifest whose project also targets that LTS
/// runtime. setup-dotnet installs SDKs only, so CI installs this SDK for its runtime.
const LTS_SDK_PIN: &str = ".dotnet-lts-sdk-version";

/// Reads the target frameworks of a .NET project: one `TargetFramework`, or a
/// `TargetFrameworks` list when the project runs on more than one runtime.
fn target_frameworks(source: &str) -> Result<Vec<String>> {
    let single = elements(source, "TargetFramework")?.len();
    let multiple = elements(source, "TargetFrameworks")?.len();
    let frameworks: Vec<String> = match (single, multiple) {
        (1, 0) => vec![element(source, "TargetFramework")?],
        (0, 1) => element(source, "TargetFrameworks")?
            .split(';')
            .map(str::to_owned)
            .collect(),
        _ => return Err("expected one SDK XML TargetFramework or TargetFrameworks".into()),
    };
    let unique: BTreeSet<&String> = frameworks.iter().collect();
    if unique.len() != frameworks.len()
        || frameworks
            .iter()
            .any(|framework| framework_major(framework).is_none())
    {
        return Err("invalid .NET target frameworks".into());
    }
    Ok(frameworks)
}

/// The runtime major version of a `netN.0` target framework.
fn framework_major(framework: &str) -> Option<u64> {
    let major = framework.strip_prefix("net")?.strip_suffix(".0")?;
    if major.is_empty() || !major.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    major.parse().ok()
}

/// Every target framework must run on a pinned runtime: the one the global.json SDK installs, or
/// the one the LTS SDK pin carries. An LTS pin that no target uses is stale.
fn runtime_coverage(frameworks: &[String], sdk: &str, lts: Option<&str>) -> Result<()> {
    let major = |pin: &str| {
        version(pin)
            .map(|(major, _, _)| major)
            .ok_or_else(|| format!("invalid .NET SDK pin {pin}"))
    };
    let sdk_major = major(sdk)?;
    let lts_major = lts.map(major).transpose()?;
    let mut targets = Vec::new();
    for framework in frameworks {
        let target = framework_major(framework)
            .ok_or_else(|| format!("unsupported .NET target framework {framework}"))?;
        if target != sdk_major && Some(target) != lts_major {
            return Err(format!(
                ".NET target framework {framework} has no pinned runtime"
            ));
        }
        targets.push(target);
    }
    if let Some(lts_major) = lts_major
        && !targets.contains(&lts_major)
    {
        return Err(format!(
            "the LTS .NET SDK pin carries runtime {lts_major}, which no target framework uses"
        ));
    }
    Ok(())
}

/// Checks that the lock holds exactly the project's target frameworks, each resolving every
/// direct pin exactly with a content hash.
fn locked_direct_pins(
    lock: &Value,
    frameworks: &[String],
    pins: &BTreeMap<String, String>,
) -> Result<()> {
    let targets = lock["dependencies"]
        .as_object()
        .ok_or("missing NuGet locked targets")?;
    if targets.len() != frameworks.len() {
        return Err("NuGet lock target frameworks differ from the project".into());
    }
    for framework in frameworks {
        // `Map` indexing panics on a missing key, and the count check above lets a lock
        // with the same number of differently named targets through.
        let entries = targets
            .get(framework.as_str())
            .and_then(Value::as_object)
            .ok_or_else(|| format!("missing NuGet locked target {framework}"))?;
        if entries
            .values()
            .filter(|entry| entry["type"] == "Direct")
            .count()
            != pins.len()
        {
            return Err("NuGet lock direct count mismatch".into());
        }
        for (name, pin) in pins {
            let entry = entries.get(name).ok_or("missing NuGet locked package")?;
            if entry["resolved"] != *pin
                || entry["requested"] != format!("[{pin}, {pin}]")
                || entry["contentHash"].as_str().is_none_or(str::is_empty)
            {
                return Err(format!("NuGet lock mismatch: {name} on {framework}"));
            }
        }
    }
    Ok(())
}

/// The newest SDK of any supported .NET release channel.
fn newest_sdk(releases: &Value) -> Result<&str> {
    releases["releases-index"]
        .as_array()
        .ok_or("missing .NET release index")?
        .iter()
        .filter(|release| supported(release))
        .filter_map(|release| {
            let pin = release["latest-sdk"].as_str()?;
            Some((version(pin)?, pin))
        })
        .max_by_key(|(numeric, _)| *numeric)
        .map(|(_, pin)| pin)
        .ok_or_else(|| "missing supported stable .NET SDK".into())
}

/// The latest SDK of the supported LTS channel that `pin` belongs to.
fn lts_channel_sdk<'a>(releases: &'a Value, pin: &str) -> Result<&'a str> {
    let (major, minor, _) = version(pin).ok_or("invalid LTS .NET SDK pin")?;
    let channel = format!("{major}.{minor}");
    let release = releases["releases-index"]
        .as_array()
        .ok_or("missing .NET release index")?
        .iter()
        .find(|release| release["channel-version"] == channel.as_str())
        .ok_or_else(|| format!("missing .NET {channel} release channel"))?;
    if release["release-type"] != "lts" {
        return Err(format!("the pinned .NET {channel} channel is not LTS"));
    }
    if !supported(release) {
        return Err(format!(
            "the pinned .NET {channel} channel is no longer supported"
        ));
    }
    text(release, "latest-sdk")
}

fn supported(release: &Value) -> bool {
    matches!(
        release["support-phase"].as_str(),
        Some("active" | "maintenance")
    )
}

fn nuget(root: &Path, scope: &Value) -> Result<bool> {
    let manifest = read(root, text(scope, "manifest")?)?;
    let pins = nuget_pins(&manifest)?;
    snapshot_pins(scope, &pins)?;
    let frameworks = target_frameworks(&manifest)?;
    locked_direct_pins(
        &json(&read_sibling(root, scope, "packages.lock.json")?)?,
        &frameworks,
        &pins,
    )?;
    let mut drift = false;
    let index = json(&fetch("https://api.nuget.org/v3/index.json", &[], false)?)?;
    let base = index["resources"]
        .as_array()
        .ok_or("missing NuGet resources")?
        .iter()
        .find(|entry| entry["@type"] == "RegistrationsBaseUrl/3.6.0")
        .and_then(|entry| entry["@id"].as_str())
        .ok_or("missing NuGet semver2 registration resource")?;
    if !base.starts_with("https://api.nuget.org/") {
        return Err("unexpected NuGet registry origin".into());
    }
    for (name, pin) in pins {
        let source = json(&fetch(
            &format!("{base}{}/index.json", name.to_ascii_lowercase()),
            &[],
            false,
        )?)?;
        let latest = latest_nuget(&source, base, &mut |url| fetch(url, &[], false))?;
        drift |= compare(&format!("nuget:{name}"), &pin, &latest);
    }
    let global = json(&read_sibling(root, scope, "global.json")?)?;
    let pin = text(&global["sdk"], "version")?;
    if global["sdk"]["rollForward"] != "disable" || scope["toolchain"]["version"] != pin {
        return Err(".NET exact toolchain snapshot mismatch".into());
    }
    let lts = optional_sibling(root, scope, LTS_SDK_PIN)?;
    let lts = lts.as_deref().map(str::trim_end);
    if lts != scope["toolchain"]["lts_sdk"].as_str() {
        return Err("LTS .NET SDK snapshot mismatch".into());
    }
    runtime_coverage(&frameworks, pin, lts)?;
    let releases = json(&fetch(
        "https://builds.dotnet.microsoft.com/dotnet/release-metadata/releases-index.json",
        &[],
        false,
    )?)?;
    drift |= compare(".NET SDK", pin, newest_sdk(&releases)?);
    if let Some(lts) = lts {
        drift |= compare(".NET LTS SDK", lts, lts_channel_sdk(&releases, lts)?);
    }
    Ok(drift)
}

#[cfg(test)]
mod tests;
