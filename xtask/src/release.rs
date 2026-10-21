use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;

const TARGETS: [&str; 8] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
];
const MUSL_SOURCE_SHA256: &str = "a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4";

fn capture(command: &mut Command) -> Result<String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|e| e.to_string())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("missing {key}"))
}

fn copy(source: &Path, destination: &Path) -> Result<()> {
    if fs::metadata(source).map_err(|e| e.to_string())?.len() == 0 {
        return Err(format!("empty distribution input: {}", source.display()));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(source, destination).map_err(|e| e.to_string())?;
    Ok(())
}

fn notice_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "COPYRIGHT",
        "AUTHORS",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

fn notice_files(base: &Path, relative: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("notice path must remain inside its package".into());
    }
    let path = base.join(relative);
    let actual = path.canonicalize().map_err(|e| e.to_string())?;
    if !actual.starts_with(base.canonicalize().map_err(|e| e.to_string())?) {
        return Err("notice path escapes its package".into());
    }
    if actual.is_file() {
        files.insert(relative.to_owned());
    } else if actual.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
                return Err("symlinks inside notice directories are unsupported".into());
            }
            notice_files(base, &relative.join(entry.file_name()), files)?;
        }
    } else {
        return Err("notice must be a regular file or directory".into());
    }
    Ok(())
}

fn package_notices(package: &Value) -> Result<(PathBuf, BTreeSet<PathBuf>)> {
    let manifest = Path::new(text(package, "manifest_path")?);
    let base = manifest
        .parent()
        .ok_or("manifest has no parent")?
        .to_owned();
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(&base).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if notice_name(&entry.file_name().to_string_lossy()) {
            notice_files(&base, Path::new(&entry.file_name()), &mut files)?;
        }
    }
    if let Some(relative) = package["license_file"].as_str() {
        let path = Path::new(relative);
        let relative = if path.is_absolute() {
            path.strip_prefix(&base)
                .map_err(|_| "license file escapes its package")?
        } else {
            path
        };
        notice_files(&base, relative, &mut files)?;
    }
    if package["name"] == "crc32c" {
        if package["version"] != "0.6.8" {
            return Err("crc32c notice exception requires review for this version".into());
        }
        for name in ["README.md", "src/combine.rs"] {
            notice_files(&base, Path::new(name), &mut files)?;
        }
    }
    if files.is_empty() {
        return Err(format!("no license notices for {}", text(package, "name")?));
    }
    Ok((base, files))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|e| e.to_string())
}

fn check_destination(target: &str, output: &Path) -> Result<()> {
    if !TARGETS.contains(&target) {
        return Err(format!("unsupported release target: {target}"));
    }
    if output.try_exists().map_err(|e| e.to_string())? || fs::symlink_metadata(output).is_ok() {
        return Err("distribution output already exists; refusing to overwrite it".into());
    }
    Ok(())
}

fn check_version(actual: &str, version: &str) -> Result<()> {
    if actual != format!("gcs-mimic {version}") {
        return Err("binary --version does not match the root package version".into());
    }
    Ok(())
}

fn native_notice_names(target: &str) -> Vec<&'static str> {
    let mut names = Vec::new();
    if target.contains("linux") {
        names.push("GCC-COPYRIGHT");
    }
    if target.ends_with("-musl") {
        names.push("musl-1.2.5.tar.gz");
    }
    names
}

fn check_native_toolchain(target: &str, rustc: &str) -> Result<()> {
    if target.ends_with("-musl") && !rustc.starts_with("rustc 1.99.1 (") {
        return Err("review the bundled musl version and notices for this Rust toolchain".into());
    }
    Ok(())
}

pub fn prepare(root: &Path, target: &str, binary: &Path, output: &Path) -> Result<()> {
    check_destination(target, output)?;
    let source_dirty = !capture(Command::new("git").current_dir(root).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ]))?
    .is_empty();
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let metadata: Value =
        serde_json::from_str(&capture(Command::new(&cargo).current_dir(root).args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ]))?)
        .map_err(|e| e.to_string())?;
    let packages = metadata["packages"].as_array().ok_or("missing packages")?;
    let product = packages
        .iter()
        .find(|package| package["id"] == metadata["resolve"]["root"])
        .ok_or("missing root package")?;
    if product["name"] != "gcs-mimic" {
        return Err("root package must be gcs-mimic".into());
    }
    let version = text(product, "version")?;
    check_version(&capture(Command::new(binary).arg("--version"))?, version)?;
    let rustc = capture(Command::new("rustc").arg("-Vv"))?;
    check_native_toolchain(target, &rustc)?;
    let cargo_version = capture(Command::new(&cargo).arg("-V"))?;
    let sha = capture(
        Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"]),
    )?;
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("source commit must be a full Git SHA".into());
    }
    let sysroot = PathBuf::from(capture(Command::new("rustc").args(["--print", "sysroot"]))?);
    let rust_notices = sysroot.join("share/doc/rust");
    let mut rust_files = BTreeSet::new();
    for name in ["COPYRIGHT-library.html", "COPYRIGHT.html", "licenses"] {
        notice_files(&rust_notices, Path::new(name), &mut rust_files)?;
    }
    let native_names = native_notice_names(target);
    let native_notices = root.join("target/release-notices");
    for name in &native_names {
        let input = native_notices.join(name);
        if !input.is_file() || fs::metadata(input).map_err(|e| e.to_string())?.len() == 0 {
            return Err(format!(
                "required native distribution notice missing: {name}"
            ));
        }
    }
    if target.ends_with("-musl") {
        let hash =
            capture(Command::new("sha256sum").arg(native_notices.join("musl-1.2.5.tar.gz")))?;
        if hash.split_whitespace().next() != Some(MUSL_SOURCE_SHA256) {
            return Err("musl source notice archive does not match the pinned SHA-256".into());
        }
    }
    let mut registry = Vec::new();
    for package in packages {
        match package["source"].as_str() {
            Some(source) if source.starts_with("registry+") || source.starts_with("sparse+") => {
                registry.push((package, package_notices(package)?));
            }
            None if ["gcs-mimic", "xtask"].contains(&text(package, "name")?) => {}
            _ => return Err("unreviewed dependency source in release metadata".into()),
        }
    }
    registry.sort_by_key(|(package, _)| (package["name"].as_str(), package["version"].as_str()));
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(output).map_err(|e| e.to_string())?;
    let binary_name = if target.contains("windows") {
        "gcs-mimic.exe"
    } else {
        "gcs-mimic"
    };
    copy(binary, &output.join(binary_name))?;
    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        copy(&root.join(name), &output.join(name))?;
    }
    let third_party = output.join("third-party-licenses");
    for name in &rust_files {
        copy(
            &rust_notices.join(name),
            &third_party.join("rust").join(name),
        )?;
    }
    for name in &native_names {
        copy(
            &native_notices.join(name),
            &third_party.join("native").join(name),
        )?;
    }
    let mut index = Vec::new();
    for (package, (base, mut files)) in registry {
        let directory = format!("{}-{}", text(package, "name")?, text(package, "version")?);
        let destination = third_party.join("crates").join(&directory);
        fs::create_dir_all(destination.parent().ok_or("missing parent")?)
            .map_err(|e| e.to_string())?;
        fs::create_dir(&destination).map_err(|e| e.to_string())?;
        for name in &files {
            copy(&base.join(name), &destination.join(name))?;
        }
        let mut license_selection = None;
        if package["name"] == "crc32c" {
            copy(
                &root.join("LICENSE-APACHE"),
                &destination.join("LICENSE-APACHE"),
            )?;
            copy(
                &root.join("distribution/notices/crc32c-0.6.8/ZLIB.txt"),
                &destination.join("ZLIB.txt"),
            )?;
            files.extend([PathBuf::from("LICENSE-APACHE"), PathBuf::from("ZLIB.txt")]);
            license_selection = Some(json!({
                "chosen_license": "Apache-2.0",
                "basis": "README.md offers Apache-2.0 OR MIT; the published crate omits standalone license texts, so the unmodified standard Apache-2.0 text is supplied.",
                "upstream_revision": "254a861f7cc71bfe455e86f0e1d86e3c83c33390",
                "adapted_source": "src/combine.rs preserves the crc32c author's Rust translation notice and Mark Adler's zlib copyright attribution; the original zlib terms are additionally preserved in ZLIB.txt.",
                "additional_license": "Zlib",
                "additional_license_source": "https://raw.githubusercontent.com/madler/zlib/v1.2.11/zlib.h",
                "review_scope": "crc32c 0.6.8 only; other versions require a fresh notice review.",
            }));
        }
        if package["name"] == "libsqlite3-sys" {
            let source =
                fs::read_to_string(base.join("sqlite3/sqlite3.c")).map_err(|e| e.to_string())?;
            let start = source
                .find("/*\n** 2001 September 15")
                .ok_or("SQLite public-domain notice missing")?;
            let end = source[start..]
                .find("*/")
                .ok_or("incomplete SQLite notice")?
                + start
                + 2;
            fs::write(
                destination.join("SQLite-PUBLIC-DOMAIN.txt"),
                &source[start..end],
            )
            .map_err(|e| e.to_string())?;
            files.insert(PathBuf::from("SQLite-PUBLIC-DOMAIN.txt"));
        }
        index.push(json!({
            "name": package["name"], "version": package["version"],
            "source": package["source"], "spdx": package["license"],
            "license_selection": license_selection,
            "directory": format!("crates/{directory}"),
            "notice_files": files.iter().map(|path| path.to_string_lossy().replace('\\', "/")).collect::<Vec<_>>(),
        }));
    }
    write_json(
        &third_party.join("INDEX.json"),
        &json!({
            "scope": "Superset of the complete locked workspace dependency graph, including build, test, host and other-target dependencies; not a claim that every listed crate is linked into this binary.",
            "rust_notices": "rust/COPYRIGHT-library.html and rust/COPYRIGHT.html are upstream toolchain attribution supersets; rust/licenses contains the associated license texts.",
            "native_notices": native_names.iter().map(|name| format!("native/{name}")).collect::<Vec<_>>(),
            "gcc_notices": if target.contains("linux") { Some("GCC-COPYRIGHT preserves the build runner GCC package's complete runtime/startup-code copyright and license terms.") } else { None },
            "musl_notices": if target.ends_with("-musl") { Some(json!({
                "version": "1.2.5",
                "source": "https://musl.libc.org/releases/musl-1.2.5.tar.gz",
                "sha256": MUSL_SOURCE_SHA256,
                "scope": "The complete upstream base source retains COPYRIGHT and individual file notices, including math and architecture-specific terms. The pinned Rust toolchain builds this base with upstream security patches; this archive is an attribution superset, not an exact reconstruction of Rust's patched build sources.",
            })) } else { None },
            "packages": index,
        }),
    )?;
    write_json(
        &output.join("BUILD-INFO.json"),
        &json!({
            "packaging_context": {
                "source_commit": sha, "source_dirty": source_dirty, "package_version": version,
                "requested_target": target, "rustc": rustc, "cargo": cargo_version,
                "runner": {
                    "os": env::var("RUNNER_OS").ok(), "architecture": env::var("RUNNER_ARCH").ok(),
                    "image_os": env::var("ImageOS").ok(), "image_version": env::var("ImageVersion").ok(),
                },
            },
            "binary_verification": {
                "version_output": format!("gcs-mimic {version}"),
                "scope": "Only the binary's --version output is verified. The checkout, compiler and requested target describe packaging context; the binary's build origin, architecture and ABI are not independently verified by this command. Release CI builds the binary immediately before packaging.",
            },
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "gcs-mimic-release-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn invalid_target_and_existing_output_are_rejected_without_mutation() {
        let directory = Directory::new();
        let marker = directory.0.join("keep");
        fs::write(&marker, "preserved").unwrap();
        assert!(check_destination("wasm32-wasip1", &directory.0.join("unused")).is_err());
        assert!(check_destination(TARGETS[0], &directory.0).is_err());
        assert_eq!(fs::read_to_string(marker).unwrap(), "preserved");
        assert!(!directory.0.join("unused").exists());
    }

    #[test]
    fn binary_version_must_match_exactly() {
        assert!(check_version("gcs-mimic 0.1.0", "0.1.0").is_ok());
        assert!(check_version("gcs-mimic 0.2.0", "0.1.0").is_err());
        assert!(check_version("another-tool 0.1.0", "0.1.0").is_err());
    }

    #[test]
    fn native_notice_requirements_follow_the_target_runtime() {
        assert!(
            check_native_toolchain("aarch64-unknown-linux-musl", "rustc 1.99.1 (hash)").is_ok()
        );
        assert!(
            check_native_toolchain("aarch64-unknown-linux-musl", "rustc 1.100.0 (hash)").is_err()
        );
        assert!(check_native_toolchain("aarch64-apple-darwin", "rustc 1.100.0 (hash)").is_ok());
        assert!(native_notice_names("aarch64-apple-darwin").is_empty());
        assert!(native_notice_names("x86_64-pc-windows-msvc").is_empty());
        assert_eq!(
            native_notice_names("x86_64-unknown-linux-gnu"),
            ["GCC-COPYRIGHT"]
        );
        assert_eq!(
            native_notice_names("aarch64-unknown-linux-musl"),
            ["GCC-COPYRIGHT", "musl-1.2.5.tar.gz"]
        );
    }

    #[test]
    fn all_root_notices_and_explicit_license_file_are_retained() {
        let directory = Directory::new();
        fs::create_dir(directory.0.join("legal")).unwrap();
        for name in [
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "NOTICE",
            "COPYRIGHT",
            "AUTHORS",
            "legal/terms.txt",
        ] {
            fs::write(directory.0.join(name), "terms").unwrap();
        }
        let package = json!({"name":"example", "manifest_path":directory.0.join("Cargo.toml"), "license_file":"legal/terms.txt"});
        let (_, files) = package_notices(&package).unwrap();
        assert_eq!(files.len(), 6);
        assert!(files.contains(Path::new("legal/terms.txt")));
        let mut absolute = package;
        absolute["license_file"] = json!(directory.0.join("legal/terms.txt"));
        assert_eq!(package_notices(&absolute).unwrap().1, files);
    }

    #[test]
    fn missing_notices_and_escaping_license_files_fail_closed() {
        let directory = Directory::new();
        let mut package = json!({"name":"example", "manifest_path":directory.0.join("Cargo.toml")});
        assert!(package_notices(&package).is_err());
        package["license_file"] = json!("../LICENSE");
        assert!(package_notices(&package).is_err());
    }

    #[test]
    fn reviewed_crc32c_sources_are_retained_and_new_versions_require_review() {
        let directory = Directory::new();
        fs::create_dir(directory.0.join("src")).unwrap();
        fs::write(
            directory.0.join("README.md"),
            "Apache-2.0 OR MIT declaration",
        )
        .unwrap();
        fs::write(
            directory.0.join("src/combine.rs"),
            "zlib attribution and adapted source",
        )
        .unwrap();
        let mut package = json!({"name":"crc32c", "version":"0.6.8", "manifest_path":directory.0.join("Cargo.toml")});
        let (_, files) = package_notices(&package).unwrap();
        assert_eq!(
            files,
            BTreeSet::from([PathBuf::from("README.md"), PathBuf::from("src/combine.rs")])
        );
        package["version"] = json!("0.6.9");
        assert!(package_notices(&package).is_err());
    }
}
