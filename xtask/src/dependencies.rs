use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, String>;
const MAX_RESPONSE: u64 = 8 * 1024 * 1024;

fn fetch(url: &str, headers: &[String], include_headers: bool) -> Result<String> {
    let mut command = Command::new("curl");
    command.args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--max-time",
        "30",
        "--max-filesize",
        "8388608",
        "--user-agent",
        "gcs-mimic-dependency-check",
    ]);
    if include_headers {
        command.arg("--include");
    }
    for header in headers {
        command.args(["--header", header]);
    }
    let mut child = command
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes);
    if bytes.len() as u64 > MAX_RESPONSE || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    read.map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(format!("response exceeds 8 MiB: {url}"));
    }
    if !status.success() {
        return Err(format!("fetch failed: {url}: {status}"));
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn scalar(source: &str, key: &str) -> Result<String> {
    let prefix = format!("{key} = \"");
    let values: Vec<_> = source
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(&prefix)
                .and_then(|v| v.strip_suffix('"'))
        })
        .collect();
    if values.len() != 1 {
        return Err(format!("expected one quoted {key}"));
    }
    Ok(values[0].to_owned())
}

fn version(text: &str) -> Option<(u64, u64, u64)> {
    let pieces: Vec<_> = text.split('.').collect();
    if pieces.len() != 3
        || pieces
            .iter()
            .any(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some((
        pieces[0].parse().ok()?,
        pieces[1].parse().ok()?,
        pieces[2].parse().ok()?,
    ))
}

#[derive(Debug, PartialEq)]
struct GoModule {
    go: String,
    toolchain: String,
    direct: BTreeMap<String, String>,
}

fn go_mod(source: &str) -> Result<GoModule> {
    let mut go = None;
    let mut toolchain = None;
    let mut direct = BTreeMap::new();
    let mut block = false;
    for line in source.lines() {
        let (code, comment) = line.split_once("//").unwrap_or((line, ""));
        let words: Vec<_> = code.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let requirement = if block {
            if words == [")"] {
                block = false;
                continue;
            }
            words.as_slice()
        } else {
            match words.as_slice() {
                ["module", _] => continue,
                ["go", value] if go.is_none() && version(value).is_some() => {
                    go = Some((*value).to_owned());
                    continue;
                }
                ["toolchain", value]
                    if toolchain.is_none()
                        && value.strip_prefix("go").and_then(version).is_some() =>
                {
                    toolchain = Some(value[2..].to_owned());
                    continue;
                }
                ["require", "("] => {
                    block = true;
                    continue;
                }
                ["require", rest @ ..] => rest,
                _ => return Err(format!("unsupported Go manifest directive: {code}")),
            }
        };
        let [name, pin] = requirement else {
            return Err(format!("invalid Go requirement: {code}"));
        };
        if comment.trim() == "indirect" {
            continue;
        }
        if go_stable_version(pin).is_none()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/.-_".contains(&b))
            || direct
                .insert((*name).to_owned(), (*pin).to_owned())
                .is_some()
        {
            return Err(format!(
                "invalid or duplicate direct Go requirement: {code}"
            ));
        }
    }
    if block || direct.is_empty() {
        return Err("unterminated or empty direct Go requirements".into());
    }
    Ok(GoModule {
        go: go.ok_or("missing exact Go version")?,
        toolchain: toolchain.ok_or("missing exact Go toolchain")?,
        direct,
    })
}

fn go_stable_version(name: &str) -> Option<(u64, u64, u64)> {
    let core = name.strip_prefix('v')?;
    version(core.strip_suffix("+incompatible").unwrap_or(core))
}

fn proxy_path(name: &str) -> String {
    name.chars()
        .flat_map(|c| {
            if c.is_ascii_uppercase() {
                vec!['!', c.to_ascii_lowercase()]
            } else {
                vec![c]
            }
        })
        .collect()
}

fn go_retractions(source: &str) -> Result<Vec<(String, String)>> {
    let mut block = false;
    let mut ranges = Vec::new();
    for line in source.lines() {
        let code = line.split_once("//").map_or(line, |(code, _)| code).trim();
        let value = if block {
            if code == ")" {
                block = false;
                continue;
            }
            code
        } else if let Some(value) = code
            .strip_prefix("retract ")
            .or_else(|| code.strip_prefix("retract\t"))
        {
            if value.trim() == "(" {
                block = true;
                continue;
            }
            value.trim()
        } else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let (low, high) =
            if let Some(range) = value.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let (low, high) = range.split_once(',').ok_or("invalid Go retraction range")?;
                (low.trim(), high.trim())
            } else {
                (value, value)
            };
        go_version_bound(low)?;
        go_version_bound(high)?;
        ranges.push((low.to_owned(), high.to_owned()));
    }
    if block {
        return Err("unterminated Go retractions".into());
    }
    Ok(ranges)
}

fn go_version_bound(name: &str) -> Result<((u64, u64, u64), bool)> {
    let name = name.strip_prefix('v').ok_or("invalid Go version prefix")?;
    let name = name.strip_suffix("+incompatible").unwrap_or(name);
    let (core, pre) = name
        .split_once('-')
        .map_or((name, None), |(v, p)| (v, Some(p)));
    if pre.is_some_and(|p| {
        p.is_empty()
            || !p
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
    }) {
        return Err("invalid Go prerelease bound".into());
    }
    Ok((version(core).ok_or("invalid Go version")?, pre.is_none()))
}

fn latest_go_module(versions: &str, manifest: &str) -> Result<String> {
    let retractions = go_retractions(manifest)?;
    let mut latest = None;
    for name in versions.lines().map(str::trim) {
        let Some(v) = go_stable_version(name) else {
            continue;
        };
        let mut retracted = false;
        for (low, high) in &retractions {
            retracted |=
                go_version_bound(low)? <= (v, true) && (v, true) <= go_version_bound(high)?;
        }
        if !retracted && latest.as_ref().is_none_or(|(current, _)| v > *current) {
            latest = Some((v, name.to_owned()));
        }
    }
    latest
        .map(|(_, name)| name)
        .ok_or("no non-retracted stable Go module version".into())
}

fn check_go(root: &Path, scope: &Value) -> Result<bool> {
    if text(scope, "ecosystem")? != "go" || text(scope, "manifest")? != "sdk-tests/go/go.mod" {
        return Err("unsupported SDK ecosystem or manifest".into());
    }
    let module = go_mod(
        &std::fs::read_to_string(root.join(text(scope, "manifest")?)).map_err(|e| e.to_string())?,
    )?;
    let releases = json(&fetch("https://go.dev/dl/?mode=json", &[], false)?)?;
    let latest = releases
        .as_array()
        .ok_or("invalid Go releases")?
        .iter()
        .filter(|r| r["stable"] == true)
        .filter_map(|r| {
            let name = r["version"].as_str()?.strip_prefix("go")?;
            Some((version(name)?, name))
        })
        .max_by_key(|(v, _)| *v)
        .map(|(_, name)| name)
        .ok_or("no stable Go release")?;
    let (major, minor, _) = version(latest).unwrap();
    let mut drift = compare(
        "Go language baseline",
        &module.go,
        &format!("{major}.{minor}.0"),
    );
    drift |= compare("Go toolchain", &module.toolchain, latest);
    for (name, current) in module.direct {
        let base = format!("https://proxy.golang.org/{}", proxy_path(&name));
        let versions = fetch(&format!("{base}/@v/list"), &[], false)?;
        let highest = versions
            .lines()
            .filter_map(|name| Some((go_stable_version(name)?, name)))
            .max_by_key(|(v, _)| *v)
            .map(|(_, name)| name)
            .ok_or("no stable Go module version")?;
        // Go loads retractions from the highest release, including a self-retracted release.
        let manifest = fetch(&format!("{base}/@v/{highest}.mod"), &[], false)?;
        let latest = latest_go_module(&versions, &manifest)?;
        drift |= compare(&name, &current, &latest);
    }
    Ok(drift)
}

fn index_path(name: &str) -> String {
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

fn latest_crate(source: &str) -> Result<String> {
    let releases = source
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    releases
        .iter()
        .filter(|r| r["yanked"] == false)
        .filter_map(|r| {
            let name = r["vers"].as_str()?;
            Some((version(name)?, name))
        })
        .max_by_key(|(v, _)| *v)
        .map(|(_, name)| name.to_owned())
        .ok_or("no non-yanked stable crate release".into())
}

fn compare(label: &str, current: &str, latest: &str) -> bool {
    let drift = current != latest;
    println!(
        "{label}: {} current={current} latest={latest}",
        if drift { "UPDATE_AVAILABLE" } else { "CURRENT" }
    );
    drift
}

fn json(source: &str) -> Result<Value> {
    serde_json::from_str(source).map_err(|e| e.to_string())
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("missing string: {key}"))
}

fn registry_response(response: &str) -> Result<(String, Value)> {
    let mut response = response;
    let mut headers = "";
    while response.starts_with("HTTP/") {
        (headers, response) = response
            .split_once("\r\n\r\n")
            .ok_or("invalid registry headers")?;
    }
    let digest = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("docker-content-digest")
                .then(|| value.trim().to_owned())
        })
        .ok_or("registry did not return a content digest")?;
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("invalid Docker digest".into());
    }
    Ok((digest, json(response)?))
}

pub fn check(root: &Path) -> Result<bool> {
    println!(
        "checked_at_unix_seconds={}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs()
    );
    let pin = scalar(
        &std::fs::read_to_string(root.join("rust-toolchain.toml")).map_err(|e| e.to_string())?,
        "channel",
    )?;
    let upstream = fetch(
        "https://static.rust-lang.org/dist/channel-rust-stable.toml",
        &[],
        false,
    )?;
    let rust = upstream
        .split("[pkg.rust]\n")
        .nth(1)
        .ok_or("missing stable Rust metadata")?
        .split("\n[")
        .next()
        .unwrap();
    let latest = scalar(rust, "version")?
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    if version(&latest).is_none() {
        return Err("invalid stable Rust version".into());
    }
    let mut drift = compare("rust", &pin, &latest);
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--locked", "--offline"])
        .env("RUSTUP_TOOLCHAIN", &pin)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("metadata packages missing")?;
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("metadata members missing")?;
    let mut names = BTreeSet::new();
    for package in packages.iter().filter(|p| members.contains(&p["id"])) {
        for dependency in package["dependencies"]
            .as_array()
            .ok_or("metadata dependencies missing")?
        {
            if dependency["source"] == "registry+https://github.com/rust-lang/crates.io-index" {
                names.insert(text(dependency, "name")?);
            }
        }
    }
    for name in names {
        let versions: BTreeSet<_> = packages
            .iter()
            .filter(|p| {
                p["name"] == name
                    && p["source"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("registry+"))
            })
            .map(|p| text(p, "version"))
            .collect::<Result<_>>()?;
        if versions.len() != 1 {
            return Err(format!("ambiguous direct resolution: {name}: {versions:?}"));
        }
        let latest = latest_crate(&fetch(
            &format!("https://index.crates.io/{}", index_path(name)),
            &[],
            false,
        )?)?;
        drift |= compare(name, versions.first().unwrap(), &latest);
    }
    let status = crate::read_json(&root.join("compatibility/dependency-status.json"))?;
    for action in status["github_actions"]
        .as_array()
        .ok_or("missing github_actions")?
    {
        let repo = text(action, "repository")?;
        let release = json(&fetch(
            &format!("https://api.github.com/repos/{repo}/releases/latest"),
            &[],
            false,
        )?)?;
        let tag = text(&release, "tag_name")?;
        let reference = json(&fetch(
            &format!("https://api.github.com/repos/{repo}/git/ref/tags/{tag}"),
            &[],
            false,
        )?)?;
        let mut object = reference["object"].clone();
        if object["type"] == "tag" {
            object = json(&fetch(text(&object, "url")?, &[], false)?)?["object"].clone();
        }
        let sha = text(&object, "sha")?;
        if object["type"] != "commit"
            || sha.len() != 40
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid action commit".into());
        }
        let pinned = text(action, "sha")?;
        for entry in std::fs::read_dir(root.join(".github/workflows")).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|s| s == "yaml" || s == "yml") {
                let workflow = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                for line in workflow.lines() {
                    if let Some(actual) = line.split(&format!("uses: {repo}@")).nth(1)
                        && actual.split_whitespace().next() != Some(pinned)
                    {
                        return Err(format!("action pin mismatch: {}", path.display()));
                    }
                }
            }
        }
        drift |= compare(repo, pinned, sha);
    }
    let dockerfile = std::fs::read_to_string(root.join("Dockerfile")).map_err(|e| e.to_string())?;
    let smoke = std::fs::read_to_string(root.join("scripts/smoke-containers.sh"))
        .map_err(|e| e.to_string())?;
    for image in status["docker"]["digests"]
        .as_array()
        .ok_or("missing Docker inputs")?
    {
        let repository = text(image, "repository")?;
        let reference = text(image, "reference")?;
        let digest = text(image, "digest")?;
        if repository == "library/debian" {
            let release = fetch(
                "https://deb.debian.org/debian/dists/stable/Release",
                &[],
                false,
            )?;
            let codename = release
                .lines()
                .find_map(|line| line.strip_prefix("Codename: "))
                .ok_or("missing Debian stable codename")?;
            drift |= compare(
                "Debian stable codename",
                text(image, "stable_codename")?,
                codename,
            );
        }
        let pin = format!("{reference}@{digest}");
        if !dockerfile.contains(&pin) && !smoke.contains(&pin) {
            return Err(format!("container pin mismatch: {reference}"));
        }
        let token = json(&fetch(
            &format!(
                "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repository}:pull"
            ),
            &[],
            false,
        )?)?;
        let headers = [format!("Authorization: Bearer {}", text(&token, "token")?), "Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json".into()];
        for tag in [text(image, "tag")?, text(image, "latest_tag")?]
            .into_iter()
            .collect::<BTreeSet<_>>()
        {
            let (actual, manifest) = registry_response(&fetch(
                &format!("https://registry-1.docker.io/v2/{repository}/manifests/{tag}"),
                &headers,
                true,
            )?)?;
            drift |= compare(&format!("docker {repository}:{tag}"), digest, &actual);
            if tag == text(image, "tag")? && actual == digest {
                for (platform, pinned) in image["platforms"]
                    .as_object()
                    .ok_or("missing platform pins")?
                {
                    let (os, arch) = platform.split_once('/').ok_or("invalid platform")?;
                    let found = manifest["manifests"]
                        .as_array()
                        .ok_or("missing image index")?
                        .iter()
                        .find(|m| {
                            m["platform"]["os"] == os && m["platform"]["architecture"] == arch
                        })
                        .ok_or("missing platform manifest")?;
                    if found["digest"] != *pinned {
                        return Err(format!("platform digest mismatch: {reference} {platform}"));
                    }
                }
            }
        }
    }
    let scopes = status["sdk_ecosystems"]
        .as_array()
        .ok_or("missing sdk_ecosystems")?;
    if scopes.len() != 1 {
        return Err("expected the declared Go SDK ecosystem".into());
    }
    drift |= check_go(root, &scopes[0])?;
    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_manifest_direct_requirements_and_exact_toolchain() {
        let source = "module local/test\ngo 1.27.3\ntoolchain go1.27.3\nrequire cloud.google.com/go/storage v1.69.0\nrequire (\n google.golang.org/api v0.306.0 // options\n example.com/transitive v0.0.0-20260101000000-abcdef // indirect\n)\n";
        let module = go_mod(source).unwrap();
        assert_eq!(module.go, "1.27.3");
        assert_eq!(module.toolchain, "1.27.3");
        assert_eq!(module.direct.len(), 2);
        assert_eq!(module.direct["cloud.google.com/go/storage"], "v1.69.0");
        assert_eq!(module.direct["google.golang.org/api"], "v0.306.0");
        assert!(go_mod(&source.replace("toolchain go1.27.3\n", "")).is_err());
        assert!(go_mod(&source.replace("go 1.27.3", "go 1.27")).is_err());
        assert!(go_mod(&source.replace("v1.69.0", "v1.70.0-rc.1")).is_err());
        assert!(go_mod(&source.replace(")\n", "")).is_err());
        assert!(go_mod(&format!("{source}replace a => b\n")).is_err());
        assert!(go_mod(&format!("{source}require google.golang.org/api v0.305.0\n")).is_err());
        assert_eq!(
            proxy_path("github.com/GoogleCloudPlatform/test"),
            "github.com/!google!cloud!platform/test"
        );
    }

    #[test]
    fn go_versions_exclude_prereleases_and_retractions() {
        let versions = "v1.9.0\nv1.10.0\nv1.11.0\nv1.12.0\nv1.13.0\nv2.0.0-rc.1\nv0.0.0-20260101000000-abcdef\n";
        assert_eq!(
            latest_go_module(versions, "module test\n").unwrap(),
            "v1.13.0"
        );
        let retractions = "retract v1.13.0 // self-retracted\nretract (\n[v1.11.0, v1.12.0]\n)\n";
        assert_eq!(latest_go_module(versions, retractions).unwrap(), "v1.10.0");
        assert_eq!(
            latest_go_module("v2.0.0+incompatible\nv1.9.0", "").unwrap(),
            "v2.0.0+incompatible"
        );
        assert_eq!(
            latest_go_module("v1.9.0\nv1.10.0", "retract [v1.10.0-rc.1, v1.10.0]").unwrap(),
            "v1.9.0"
        );
        assert_eq!(
            latest_go_module("v1.9.0\nv1.10.0", "retract [v1.9.0, v1.10.0-rc.1]").unwrap(),
            "v1.10.0"
        );
        assert!(latest_go_module("v1.0.0", "retract v1.0.0").is_err());
        assert!(latest_go_module("v2.0.0-beta", "").is_err());
        assert!(latest_go_module(versions, "retract (\nv1.9.0").is_err());
        assert!(latest_go_module(versions, "retract [v1.0.0]").is_err());
        assert!(latest_go_module(versions, "retract invalid").is_err());
    }

    #[test]
    fn stable_major_selection_and_registry_paths() {
        let source = "{\"vers\":\"1.9.0\",\"yanked\":false}\n{\"vers\":\"2.0.0\",\"yanked\":false}\n{\"vers\":\"3.0.0-beta.1\",\"yanked\":false}\n{\"vers\":\"2.1.0\",\"yanked\":true}";
        assert_eq!(latest_crate(source).unwrap(), "2.0.0");
        assert!(latest_crate("{\"vers\":\"1.0.0\",\"yanked\":true}").is_err());
        assert!(latest_crate("malformed").is_err());
        for (name, path) in [
            ("a", "1/a"),
            ("ab", "2/ab"),
            ("abc", "3/a/abc"),
            ("serde_json", "se/rd/serde_json"),
        ] {
            assert_eq!(index_path(name), path);
        }
        assert!(!compare("test", "1.0.0", "1.0.0"));
        assert!(compare("test", "1.0.0", "2.0.0"));
    }

    #[test]
    fn metadata_and_proxy_headers_fail_closed() {
        assert_eq!(scalar("channel = \"1.99.1\"", "channel").unwrap(), "1.99.1");
        assert!(scalar("channel = \"a\"\nchannel = \"b\"", "channel").is_err());
        let digest = format!("sha256:{}", "a".repeat(64));
        let response = format!(
            "HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 200\r\nDocker-Content-Digest: {digest}\r\n\r\n{{\"manifests\":[]}}"
        );
        assert_eq!(registry_response(&response).unwrap().0, digest);
        assert!(registry_response("HTTP/2 200\r\n\r\n{}").is_err());
        assert!(registry_response("bad").is_err());
    }
}
