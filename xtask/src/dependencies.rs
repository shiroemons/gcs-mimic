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
    let (status, body) = fetch_response(url, headers, include_headers)?;
    if !(200..300).contains(&status) {
        return Err(format!("fetch failed: {url}: HTTP {status}"));
    }
    Ok(body)
}

// Unauthenticated GitHub API calls share a small per-IP limit on hosted runners.
// The prefix check keeps the token away from response-supplied URLs on other hosts.
fn github_api_headers(url: &str, token: Option<&str>) -> Vec<String> {
    match token.map(str::trim) {
        Some(token) if !token.is_empty() && url.starts_with("https://api.github.com/") => {
            vec![format!("Authorization: Bearer {token}")]
        }
        _ => Vec::new(),
    }
}

fn fetch_github_api(url: &str) -> Result<String> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    fetch(url, &github_api_headers(url, token.as_deref()), false)
}

fn fetch_response(url: &str, headers: &[String], include_headers: bool) -> Result<(u16, String)> {
    let mut command = Command::new("curl");
    command.args([
        "--disable",
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
        "--write-out",
        "\n%{http_code}",
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
    let response = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let (body, status) = response.rsplit_once('\n').ok_or("missing HTTP status")?;
    Ok((
        status.parse().map_err(|_| "invalid HTTP status")?,
        body.to_owned(),
    ))
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

pub(crate) fn version(text: &str) -> Option<(u64, u64, u64)> {
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

fn go_proxy_response(status: u16, body: String) -> Result<Option<String>> {
    match status {
        200 => Ok(Some(body)),
        404 | 410 => Ok(None),
        _ => Err(format!("Go module proxy returned HTTP {status}")),
    }
}

fn latest_go_dependency(
    name: &str,
    mut fetch: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<String> {
    let (root, major) = match name.rsplit_once("/v") {
        Some((root, suffix)) if suffix.parse::<u64>().is_ok_and(|n| n >= 2) => {
            (root, suffix.parse::<u64>().unwrap())
        }
        _ => (name, 1),
    };
    let mut path = name.to_owned();
    let mut major = major;
    let mut latest = None;
    loop {
        let base = format!("https://proxy.golang.org/{}", proxy_path(&path));
        let Some(versions) = fetch(&format!("{base}/@v/list"))? else {
            return latest.ok_or_else(|| format!("missing Go module: {name}"));
        };
        let highest = versions
            .lines()
            .filter_map(|name| Some((go_stable_version(name)?, name)))
            .max_by_key(|(v, _)| *v)
            .map(|(_, name)| name);
        if let Some(highest) = highest {
            // Go loads retractions from the highest release, including self-retractions.
            let manifest =
                fetch(&format!("{base}/@v/{highest}.mod"))?.ok_or("missing Go module manifest")?;
            let candidate = latest_go_module(&versions, &manifest)?;
            if latest.as_ref().is_none_or(|current: &String| {
                go_stable_version(&candidate) > go_stable_version(current)
            }) {
                latest = Some(candidate);
            }
        }
        major = major.checked_add(1).ok_or("Go major version overflow")?;
        path = format!("{root}/v{major}");
    }
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
        let latest = latest_go_dependency(&name, |url| {
            let (status, body) = fetch_response(url, &[], false)?;
            go_proxy_response(status, body).map_err(|error| format!("{url}: {error}"))
        })?;
        drift |= compare(&name, &current, &latest);
    }
    Ok(drift)
}

fn ruby_manifest(source: &str) -> Result<BTreeMap<String, String>> {
    let mut direct = BTreeMap::new();
    let mut registry = false;
    for line in source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.starts_with('#') || line == "ruby file: \".ruby-version\"" {
            continue;
        }
        if line == "source \"https://rubygems.org\"" && !registry {
            registry = true;
            continue;
        }
        let (name, pin) = line
            .strip_prefix("gem \"")
            .and_then(|line| line.strip_suffix('"'))
            .and_then(|line| line.split_once("\", \"= "))
            .ok_or_else(|| format!("unsupported Ruby manifest directive: {line}"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || gem_version(pin).is_none()
            || direct.insert(name.to_owned(), pin.to_owned()).is_some()
        {
            return Err(format!(
                "invalid or duplicate direct Ruby requirement: {line}"
            ));
        }
    }
    if !registry || direct.is_empty() {
        return Err("missing RubyGems registry or direct Ruby requirements".into());
    }
    Ok(direct)
}

fn latest_ruby(source: &str) -> Result<&str> {
    let mut releases = source.lines().filter_map(|line| {
        line.strip_prefix("The current stable version is ")?
            .strip_suffix('.')
    });
    let latest = releases.next().ok_or("missing stable Ruby release")?;
    if version(latest).is_none() || releases.next().is_some() {
        return Err("invalid stable Ruby release".into());
    }
    Ok(latest)
}

fn latest_gem(source: &str) -> Result<String> {
    let releases = json(source)?;
    releases
        .as_array()
        .ok_or("invalid RubyGems versions")?
        .iter()
        .filter(|release| release["prerelease"] == false && release["yanked"] != true)
        .filter_map(|release| {
            let name = release["number"].as_str()?;
            Some((gem_version(name)?, name))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, name)| name.to_owned())
        .ok_or("no non-yanked stable gem release".into())
}

fn gem_version(pin: &str) -> Option<Vec<u64>> {
    pin.split('.')
        .map(|piece| {
            if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            piece.parse().ok()
        })
        .collect()
}

fn check_ruby(root: &Path, scope: &Value) -> Result<bool> {
    let directory = ruby_directory(root, text(scope, "manifest")?)?;
    let direct = ruby_manifest(
        &std::fs::read_to_string(directory.join("Gemfile")).map_err(|e| e.to_string())?,
    )?;
    let current =
        std::fs::read_to_string(directory.join(".ruby-version")).map_err(|e| e.to_string())?;
    if version(current.trim()).is_none() {
        return Err("invalid pinned Ruby version".into());
    }
    let releases = fetch("https://www.ruby-lang.org/en/downloads.md", &[], false)?;
    let mut drift = compare("Ruby toolchain", current.trim(), latest_ruby(&releases)?);
    let lock =
        std::fs::read_to_string(directory.join("Gemfile.lock")).map_err(|e| e.to_string())?;
    let bundler = lock
        .split_once("\nBUNDLED WITH\n")
        .map(|(_, pin)| pin.trim())
        .filter(|pin| gem_version(pin).is_some())
        .ok_or("missing exact Bundler lockfile pin")?;
    for (name, current) in direct
        .iter()
        .map(|(name, pin)| (name.as_str(), pin.as_str()))
        .chain(std::iter::once(("bundler", bundler)))
    {
        if name != "bundler" && !ruby_lock_pin(&lock, name, current) {
            return Err(format!("Ruby lockfile pin mismatch: {name}"));
        }
        let latest = latest_gem(&fetch(
            &format!("https://rubygems.org/api/v1/versions/{name}.json"),
            &[],
            false,
        )?)?;
        drift |= compare(name, current, &latest);
    }
    Ok(drift)
}

fn ruby_lock_pin(lock: &str, name: &str, current: &str) -> bool {
    let prefix = format!("    {name} (");
    let mut found = false;
    for resolved in lock
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix)?.strip_suffix(')'))
    {
        let (version, platform) = resolved.split_once('-').unwrap_or((resolved, "ruby"));
        if version != current || platform.is_empty() {
            return false;
        }
        found = true;
    }
    found
}

fn ruby_directory(root: &Path, manifest: &str) -> Result<std::path::PathBuf> {
    match manifest {
        "sdk-tests/ruby/Gemfile" | "sdk-tests/ruby/active_storage/Gemfile" => {
            Ok(root.join(manifest).parent().unwrap().to_owned())
        }
        _ => Err("unsupported Ruby SDK manifest".into()),
    }
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

fn registry_manifest_source(
    image: &Value,
    mut get: impl FnMut(&str) -> Result<String>,
) -> Result<(String, Vec<String>)> {
    let repository = text(image, "repository")?;
    let registry = if image.get("registry").is_some() {
        text(image, "registry")?
    } else {
        "docker.io"
    };
    let mut headers = vec!["Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json".into()];
    let host = match registry {
        "docker.io" => {
            let token = json(&get(&format!(
                "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repository}:pull"
            ))?)?;
            headers.push(format!("Authorization: Bearer {}", text(&token, "token")?));
            "registry-1.docker.io"
        }
        "gcr.io" => "gcr.io",
        _ => return Err(format!("unsupported image registry: {registry}")),
    };
    Ok((
        format!("https://{host}/v2/{repository}/manifests/"),
        headers,
    ))
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

fn direct_crate_resolutions(metadata: &Value) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("metadata packages missing")?;
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("metadata members missing")?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("metadata resolve nodes missing")?;
    let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for node in nodes.iter().filter(|node| members.contains(&node["id"])) {
        for dependency in node["deps"]
            .as_array()
            .ok_or("metadata resolved dependencies missing")?
        {
            let package = packages
                .iter()
                .find(|package| package["id"] == dependency["pkg"])
                .ok_or("resolved dependency package missing")?;
            if package["source"] == "registry+https://github.com/rust-lang/crates.io-index" {
                direct
                    .entry(text(package, "name")?.to_owned())
                    .or_default()
                    .insert(text(package, "version")?.to_owned());
            }
        }
    }
    Ok(direct)
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
    for (name, versions) in direct_crate_resolutions(&metadata)? {
        if versions.len() != 1 {
            return Err(format!("ambiguous direct resolution: {name}: {versions:?}"));
        }
        let latest = latest_crate(&fetch(
            &format!("https://index.crates.io/{}", index_path(&name)),
            &[],
            false,
        )?)?;
        drift |= compare(&name, versions.first().unwrap(), &latest);
    }
    let status = crate::read_json(&root.join("compatibility/dependency-status.json"))?;
    for action in status["github_actions"]
        .as_array()
        .ok_or("missing github_actions")?
    {
        let repo = text(action, "repository")?;
        let release = json(&fetch_github_api(&format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))?)?;
        let tag = text(&release, "tag_name")?;
        let reference = json(&fetch_github_api(&format!(
            "https://api.github.com/repos/{repo}/git/ref/tags/{tag}"
        ))?)?;
        let mut object = reference["object"].clone();
        if object["type"] == "tag" {
            object = json(&fetch_github_api(text(&object, "url")?)?)?["object"].clone();
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
    let mut container_sources = format!("{dockerfile}\n{smoke}");
    for entry in std::fs::read_dir(root.join(".github/workflows")).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|s| s == "yaml" || s == "yml") {
            container_sources.push_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?);
        }
    }
    for image in status["docker"]["digests"]
        .as_array()
        .ok_or("missing Docker inputs")?
    {
        let repository = text(image, "repository")?;
        let reference = text(image, "reference")?;
        let digest = text(image, "digest")?;
        if image.get("stable_codename").is_some() {
            let expected = text(image, "stable_codename")?;
            let release = fetch(
                "https://deb.debian.org/debian/dists/stable/Release",
                &[],
                false,
            )?;
            let codename = release
                .lines()
                .find_map(|line| line.strip_prefix("Codename: "))
                .ok_or("missing Debian stable codename")?;
            drift |= compare("Debian stable codename", expected, codename);
        }
        let pin = format!("{reference}@{digest}");
        if !container_sources.contains(&pin) {
            return Err(format!("container pin mismatch: {reference}"));
        }
        let (manifest_source, headers) =
            registry_manifest_source(image, |url| fetch(url, &[], false))?;
        for tag in [text(image, "tag")?, text(image, "latest_tag")?]
            .into_iter()
            .collect::<BTreeSet<_>>()
        {
            let (actual, manifest) =
                registry_response(&fetch(&format!("{manifest_source}{tag}"), &headers, true)?)?;
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
    let mut ecosystems = BTreeSet::new();
    let mut manifests = BTreeSet::new();
    for scope in scopes {
        let ecosystem = text(scope, "ecosystem")?;
        if !manifests.insert(text(scope, "manifest")?) {
            return Err("duplicate SDK manifest".into());
        }
        ecosystems.insert(ecosystem);
        drift |= match ecosystem {
            "go" => check_go(root, scope)?,
            "ruby" => check_ruby(root, scope)?,
            _ => return Err(format!("unsupported SDK ecosystem: {ecosystem}")),
        };
    }
    if !ecosystems.contains("go") || !ecosystems.contains("ruby") {
        return Err("expected the declared Go and Ruby SDK ecosystems".into());
    }
    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_resolution_excludes_older_transitive_packages_with_the_same_name() {
        let metadata = serde_json::json!({
            "workspace_members": ["app"],
            "packages": [
                {"id":"app", "source":null},
                {"id":"direct", "name":"getrandom", "version":"0.4.3", "source":"registry+https://github.com/rust-lang/crates.io-index"},
                {"id":"transitive", "name":"getrandom", "version":"0.2.17", "source":"registry+https://github.com/rust-lang/crates.io-index"}
            ],
            "resolve": {"nodes":[
                {"id":"app", "deps":[{"pkg":"direct"}]},
                {"id":"direct", "deps":[{"pkg":"transitive"}]}
            ]}
        });
        assert_eq!(
            direct_crate_resolutions(&metadata).unwrap()["getrandom"],
            BTreeSet::from(["0.4.3".to_owned()])
        );
        let mut invalid = metadata;
        invalid["resolve"]["nodes"][0]["deps"][0]["pkg"] = "missing".into();
        assert!(direct_crate_resolutions(&invalid).is_err());
    }

    #[test]
    fn ruby_lock_pins_include_all_native_platform_variants() {
        let lock = "    sqlite3 (2.10.2-arm64-darwin)\n    sqlite3 (2.10.2-x86_64-linux-gnu)\n";
        assert!(ruby_lock_pin(lock, "sqlite3", "2.10.2"));
        assert!(!ruby_lock_pin(lock, "sqlite3", "2.10.1"));
        assert!(!ruby_lock_pin(lock, "other", "2.10.2"));
        assert!(!ruby_lock_pin(
            &format!("{lock}    sqlite3 (2.10.1)\n"),
            "sqlite3",
            "2.10.2"
        ));
        assert!(ruby_lock_pin("    sqlite3 (2.10.2)\n", "sqlite3", "2.10.2"));
    }

    #[test]
    fn ruby_scopes_include_active_storage_without_arbitrary_paths() {
        let root = Path::new("checkout");
        assert_eq!(
            ruby_directory(root, "sdk-tests/ruby/active_storage/Gemfile").unwrap(),
            root.join("sdk-tests/ruby/active_storage")
        );
        assert!(ruby_directory(root, "../Gemfile").is_err());
        assert!(ruby_directory(root, "sdk-tests/ruby/other/Gemfile").is_err());
    }

    #[test]
    fn ruby_manifest_requires_exact_registry_pins() {
        let source = "source \"https://rubygems.org\"\nruby file: \".ruby-version\"\ngem \"google-cloud-storage\", \"= 1.62.1\"\n";
        let direct = ruby_manifest(source).unwrap();
        assert_eq!(direct["google-cloud-storage"], "1.62.1");
        for invalid in [
            source.replace("= 1.62.1", "~> 1.62"),
            source.replace("1.62.1", "2.0.0.rc1"),
            source.replace("https://rubygems.org", "https://other.invalid"),
            format!("{source}gem \"google-cloud-storage\", \"= 1.62.1\"\n"),
            format!("{source}eval_gemfile \"other\"\n"),
        ] {
            assert!(ruby_manifest(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn ruby_and_gem_latest_versions_include_new_majors() {
        assert_eq!(
            latest_ruby("The current stable version is 4.0.9.\n").unwrap(),
            "4.0.9"
        );
        for invalid in [
            "",
            "The current stable version is 4.1.0-preview1.",
            "The current stable version is 4.0.9.\nThe current stable version is 4.1.0.",
        ] {
            assert!(latest_ruby(invalid).is_err());
        }
        let versions = r#"[
            {"number":"1.62.1","prerelease":false},
            {"number":"2.0.0.1","prerelease":false},
            {"number":"3.0.0.pre1","prerelease":true},
            {"number":"2.1.0","prerelease":false,"yanked":true}
        ]"#;
        assert_eq!(latest_gem(versions).unwrap(), "2.0.0.1");
        for invalid in [
            "{}",
            "[]",
            "bad",
            r#"[{"number":"2.0.0","prerelease":true}]"#,
        ] {
            assert!(latest_gem(invalid).is_err());
        }
    }

    #[test]
    fn go_new_major_paths_are_checked_until_missing() {
        for start in ["example.com/SDK", "example.com/SDK/v2"] {
            let mut requests = Vec::new();
            let latest = latest_go_dependency(start, |url| {
                requests.push(url.to_owned());
                let response = match url
                    .strip_prefix("https://proxy.golang.org/example.com/!s!d!k")
                    .unwrap()
                {
                    "/@v/list" => "v1.9.0\n",
                    "/@v/v1.9.0.mod" => "module example.com/SDK\n",
                    "/v2/@v/list" => "v2.0.0\nv2.1.0\n",
                    "/v2/@v/v2.1.0.mod" => "module example.com/SDK/v2\nretract v2.1.0\n",
                    "/v3/@v/list" => "v3.0.0\nv3.1.0-rc.1\n",
                    "/v3/@v/v3.0.0.mod" => "module example.com/SDK/v3\n",
                    "/v4/@v/list" => return Ok(None),
                    path => panic!("unexpected proxy path: {path}"),
                };
                Ok(Some(response.to_owned()))
            })
            .unwrap();
            assert_eq!(latest, "v3.0.0");
            assert_eq!(requests.len(), if start.ends_with("/v2") { 5 } else { 7 });
            assert!(requests.last().unwrap().ends_with("/v4/@v/list"));
        }
    }

    #[test]
    fn go_missing_majors_and_proxy_failures_are_distinct() {
        for missing in [404, 410] {
            let latest = latest_go_dependency("example.com/sdk", |url| {
                if url.ends_with("/v2/@v/list") {
                    go_proxy_response(missing, "not found".into())
                } else if url.ends_with(".mod") {
                    Ok(Some("module example.com/sdk".into()))
                } else {
                    go_proxy_response(200, "v0.9.0\nv1.0.0".into())
                }
            })
            .unwrap();
            assert_eq!(latest, "v1.0.0");
        }
        for status in [401, 403, 429, 500, 503] {
            assert!(
                latest_go_dependency("example.com/sdk", |_| {
                    go_proxy_response(status, "failure".into())
                })
                .is_err()
            );
        }
        assert!(latest_go_dependency("example.com/sdk", |_| Err("timeout".into())).is_err());
        assert!(latest_go_dependency("example.com/sdk", |_| Ok(None)).is_err());
        assert!(
            latest_go_dependency("example.com/sdk", |url| {
                if url.ends_with("/v2/@v/list") {
                    return Err("new major lookup failed".into());
                }
                Ok(Some(
                    if url.ends_with(".mod") {
                        "module example.com/sdk"
                    } else {
                        "v1.0.0"
                    }
                    .into(),
                ))
            })
            .is_err()
        );
    }

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
    fn github_token_is_sent_only_to_the_api_host() {
        let api = "https://api.github.com/repos/actions/checkout/releases/latest";
        assert_eq!(
            github_api_headers(api, Some("test-token")),
            ["Authorization: Bearer test-token"]
        );
        assert!(github_api_headers(api, None).is_empty());
        assert!(github_api_headers(api, Some(" \n")).is_empty());
        for url in [
            "https://github.com/actions/checkout",
            "https://api.github.com.example/repos/a/b",
            "http://api.github.com/repos/a/b",
            "https://proxy.golang.org/",
        ] {
            assert!(
                github_api_headers(url, Some("test-token")).is_empty(),
                "{url}"
            );
        }
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

    #[test]
    fn docker_hub_registry_default_preserves_token_authentication() {
        for source in [
            r#"{"repository":"library/rust"}"#,
            r#"{"registry":"docker.io","repository":"library/rust"}"#,
        ] {
            let (url, headers) = registry_manifest_source(&json(source).unwrap(), |url| {
                assert_eq!(url, "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/rust:pull");
                Ok(r#"{"token":"test-token"}"#.into())
            })
            .unwrap();
            assert_eq!(
                url,
                "https://registry-1.docker.io/v2/library/rust/manifests/"
            );
            assert!(
                headers
                    .iter()
                    .any(|header| header == "Authorization: Bearer test-token")
            );
        }
    }

    #[test]
    fn public_gcr_registry_does_not_request_or_send_authentication() {
        let image = json(r#"{"registry":"gcr.io","repository":"distroless/cc-debian13"}"#).unwrap();
        let (url, headers) = registry_manifest_source(&image, |_| {
            panic!("public GCR must not request a Docker Hub token")
        })
        .unwrap();
        assert_eq!(url, "https://gcr.io/v2/distroless/cc-debian13/manifests/");
        assert_eq!(headers.len(), 1);
        assert!(headers[0].starts_with("Accept: application/vnd.oci.image.index.v1+json"));
    }

    #[test]
    fn unknown_or_malformed_registry_is_rejected_before_network_access() {
        for registry in [r#""unknown.example""#, "null", "42", "true"] {
            let image = json(&format!(
                r#"{{"registry":{registry},"repository":"distroless/cc-debian13"}}"#
            ))
            .unwrap();
            assert!(
                registry_manifest_source(&image, |_| {
                    panic!("invalid registry must not trigger authentication")
                })
                .is_err()
            );
        }
    }

    #[test]
    fn gcr_oci_index_and_lowercase_digest_headers_are_preserved() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let body = format!(
            r#"{{"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"digest":"{digest}","platform":{{"os":"linux","architecture":"arm64"}}}}]}}"#
        );
        let response = format!(
            "HTTP/2 200 \r\ncontent-type: application/vnd.oci.image.index.v1+json\r\ndocker-content-digest: {digest}\r\n\r\n{body}"
        );
        let (actual, manifest) = registry_response(&response).unwrap();
        assert_eq!(actual, digest);
        assert_eq!(manifest["manifests"][0]["digest"], digest);
        assert_eq!(
            manifest["manifests"][0]["platform"]["architecture"],
            "arm64"
        );
    }
}
