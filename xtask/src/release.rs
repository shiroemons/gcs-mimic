use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

pub(crate) mod version;
pub use version::InvalidVersion;

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
const RUST_NOTICES: [&str; 2] = ["COPYRIGHT-library.html", "licenses"];
const MUSL_SOURCE_SHA256: &str = "a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4";
// These MIT crates were published from one grpc-rust revision without their license text. The
// supplied text is that revision's repository-root LICENSE, which tonic 0.14.9 packages verbatim.
const GRPC_RUST_REVISION: &str = "4c0f455c264fe2ec9f9ab5d0cc7fd2824d5daf70";
const GRPC_RUST_LICENSE: &str = "distribution/notices/grpc-rust-4c0f455c/LICENSE";
const GRPC_RUST_SUPPLIED: [&str; 2] = ["tonic-prost", "tonic-prost-build"];
const GRPC_RUST_VERSION: &str = "0.14.9";
// crc32c adapts zlib's CRC combine code, so its notices need a per-version review on top of the
// crate's own license files; ZLIB.txt keeps the original zlib terms that combine.rs points to.
const CRC32C_REVIEWED_VERSION: &str = "0.6.9";
const CRC32C_ZLIB_NOTICE: &str = "distribution/notices/crc32c-0.6.9/ZLIB.txt";

fn grpc_rust_supplied(package: &Value) -> Result<bool> {
    if !GRPC_RUST_SUPPLIED.contains(&text(package, "name")?) {
        return Ok(false);
    }
    if package["version"] != GRPC_RUST_VERSION {
        return Err(format!(
            "{} notice exception requires review for this version",
            text(package, "name")?
        ));
    }
    Ok(true)
}

pub(crate) fn capture(command: &mut Command) -> Result<String> {
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
        if package["version"] != CRC32C_REVIEWED_VERSION {
            return Err("crc32c notice exception requires review for this version".into());
        }
        // The crate's own license texts cover its MIT OR Apache-2.0 terms; combine.rs carries the
        // zlib attribution for the adapted CRC combine code, which no license file repeats.
        notice_files(&base, Path::new("src/combine.rs"), &mut files)?;
    }
    if package["name"] == "libmimalloc-sys" {
        // The crate's own license text covers its bindings. The allocator compiled into a musl
        // binary is Microsoft's, whose copyright only this file states.
        notice_files(&base, Path::new("c_src/mimalloc/v3/LICENSE"), &mut files)?;
    }
    if files.is_empty() && !grpc_rust_supplied(package)? {
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

#[derive(Debug)]
enum BinaryVersionError {
    Invalid(InvalidVersion),
    Mismatch,
}

impl std::fmt::Display for BinaryVersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Mismatch => {
                f.write_str("binary --version does not match the root package version")
            }
        }
    }
}

impl std::error::Error for BinaryVersionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(error) => Some(error),
            Self::Mismatch => None,
        }
    }
}

fn check_version(actual: &str, version: &str) -> std::result::Result<(), BinaryVersionError> {
    let version: version::Version = version.parse().map_err(BinaryVersionError::Invalid)?;
    if actual != format!("gcs-mimic {version}") {
        return Err(BinaryVersionError::Mismatch);
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
    if target.ends_with("-musl") && !rustc.starts_with("rustc 1.100.0 (") {
        return Err("review the bundled musl version and notices for this Rust toolchain".into());
    }
    Ok(())
}

fn cargo_metadata(root: &Path) -> Result<Value> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    serde_json::from_str(&capture(Command::new(cargo).current_dir(root).args([
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1",
    ]))?)
    .map_err(|e| e.to_string())
}

pub fn prepare(
    root: &Path,
    target: &str,
    binary: &Path,
    output: &Path,
) -> std::result::Result<(), Box<dyn Error>> {
    check_destination(target, output)?;
    let source_dirty = !capture(Command::new("git").current_dir(root).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ]))?
    .is_empty();
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let metadata = cargo_metadata(root)?;
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
    write_notices(root, target, &metadata, output)?;
    let binary_name = if target.contains("windows") {
        "gcs-mimic.exe"
    } else {
        "gcs-mimic"
    };
    copy(binary, &output.join(binary_name))?;
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

// The container ships one static musl binary on an empty base image, so its notices must name
// the musl runtime the binary links rather than whatever host built it.
fn check_container_target(target: &str) -> Result<()> {
    if !target.ends_with("-unknown-linux-musl") {
        return Err(format!(
            "container notices require a static Linux musl target: {target}"
        ));
    }
    Ok(())
}

pub fn notices(root: &Path, target: &str, output: &Path) -> Result<()> {
    check_container_target(target)?;
    check_destination(target, output)?;
    check_native_toolchain(target, &capture(Command::new("rustc").arg("-Vv"))?)?;
    let metadata = cargo_metadata(root)?;
    write_notices(root, target, &metadata, output)?;
    let index_path = output.join("third-party-licenses/INDEX.json");
    let mut index: Value =
        serde_json::from_slice(&fs::read(&index_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    index["runtime_os_notices"] = json!({
        "scope": "The runtime image starts from scratch and ships no operating-system packages; the statically linked C runtime's notices are listed under native_notices.",
    });
    write_json(&index_path, &index)
}

// Workspace members are first-party sources; any other package must come from a registry so its
// notices can be collected. Path and Git dependencies outside the workspace stay unreviewed.
fn third_party_packages(metadata: &Value) -> Result<Vec<&Value>> {
    let packages = metadata["packages"].as_array().ok_or("missing packages")?;
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("missing workspace members")?;
    let mut registry = Vec::new();
    for package in packages {
        match package["source"].as_str() {
            Some(source) if source.starts_with("registry+") || source.starts_with("sparse+") => {
                registry.push(package);
            }
            None if members.contains(&package["id"]) => {}
            _ => return Err("unreviewed dependency source in release metadata".into()),
        }
    }
    Ok(registry)
}

fn write_notices(root: &Path, target: &str, metadata: &Value, output: &Path) -> Result<()> {
    let sysroot = PathBuf::from(capture(Command::new("rustc").args(["--print", "sysroot"]))?);
    let rust_notices = sysroot.join("share/doc/rust");
    let mut rust_files = BTreeSet::new();
    // Binaries link only the standard library, which COPYRIGHT-library.html covers. The 14 MB
    // COPYRIGHT.html covers the whole toolchain (rustc, LLVM and tools), none of which ships.
    for name in RUST_NOTICES {
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
    let mut registry = third_party_packages(metadata)?
        .into_iter()
        .map(|package| Ok((package, package_notices(package)?)))
        .collect::<Result<Vec<_>>>()?;
    registry.sort_by_key(|(package, _)| (package["name"].as_str(), package["version"].as_str()));
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(output).map_err(|e| e.to_string())?;
    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        copy(&root.join(name), &output.join(name))?;
    }
    let third_party = output.join("third-party-licenses");
    fs::create_dir_all(third_party.join("mime")).map_err(|e| e.to_string())?;
    fs::write(
        third_party.join("mime/NOTICE"),
        include_str!("../../src/mime.NOTICE"),
    )
    .map_err(|e| e.to_string())?;
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
                &root.join(CRC32C_ZLIB_NOTICE),
                &destination.join("ZLIB.txt"),
            )?;
            files.insert(PathBuf::from("ZLIB.txt"));
            license_selection = Some(json!({
                "basis": "Cargo.toml declares MIT OR Apache-2.0 and the published crate ships both LICENSE-MIT and LICENSE-APACHE, which are retained unchanged.",
                "upstream_revision": "3098a6edd7b1cb28198e3048889907b7160043e4",
                "adapted_source": "src/combine.rs preserves the crc32c author's Rust translation notice and Mark Adler's zlib copyright attribution; the original zlib terms are additionally preserved in ZLIB.txt.",
                "additional_license": "Zlib",
                "additional_license_source": "https://raw.githubusercontent.com/madler/zlib/v1.2.11/zlib.h",
                "review_scope": format!("crc32c {CRC32C_REVIEWED_VERSION} only; other versions require a fresh notice review."),
            }));
        }
        if grpc_rust_supplied(package)? {
            copy(&root.join(GRPC_RUST_LICENSE), &destination.join("LICENSE"))?;
            files.insert(PathBuf::from("LICENSE"));
            license_selection = Some(json!({
                "chosen_license": "MIT",
                "basis": "The published crate declares MIT but omits the license text, so LICENSE is the repository-root text from the revision it was published from.",
                "upstream_revision": GRPC_RUST_REVISION,
                "upstream_source": format!("https://raw.githubusercontent.com/grpc/grpc-rust/{GRPC_RUST_REVISION}/LICENSE"),
                "review_scope": "tonic-prost and tonic-prost-build 0.14.9 only; other versions require a fresh notice review.",
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
            "rust_notices": "rust/COPYRIGHT-library.html is the upstream attribution for the Rust standard library linked into the binary; rust/licenses contains the associated license texts. The toolchain-wide COPYRIGHT.html is omitted because no compiler or tool component ships.",
            "mime_notices": "mime/NOTICE retains the pinned Go BSD-3-Clause and mailcap Public Domain attribution for the embedded seed MIME profile.",
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) struct Directory(pub(super) PathBuf);
    impl Directory {
        pub(super) fn new() -> Self {
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
    fn notices_cover_registry_packages_and_reject_sources_outside_the_workspace() {
        let member = |name: &str| json!({"id": format!("path+file:///repo#{name}@0.1.0"), "name": name, "source": null});
        let registry = json!({"id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0", "name": "serde", "source": "registry+https://github.com/rust-lang/crates.io-index"});
        // Every workspace member, including generation-only tools, is first-party.
        let mut metadata = json!({
            "packages": [member("gcs-mimic"), member("xtask"), member("grpc-codegen"), registry],
            "workspace_members": ["path+file:///repo#gcs-mimic@0.1.0", "path+file:///repo#xtask@0.1.0", "path+file:///repo#grpc-codegen@0.1.0"],
        });
        let names = |packages: Vec<&Value>| {
            packages
                .iter()
                .map(|package| package["name"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(third_party_packages(&metadata).unwrap()),
            [json!("serde")]
        );
        let unreviewed = [
            member("vendored"),
            json!({"id": "git+https://example.invalid/x#x@0.1.0", "name": "x", "source": "git+https://example.invalid/x"}),
        ];
        for package in unreviewed {
            metadata["packages"].as_array_mut().unwrap().push(package);
            assert_eq!(
                third_party_packages(&metadata).err().as_deref(),
                Some("unreviewed dependency source in release metadata")
            );
            metadata["packages"].as_array_mut().unwrap().pop();
        }
        metadata
            .as_object_mut()
            .unwrap()
            .remove("workspace_members");
        assert!(third_party_packages(&metadata).is_err());
    }

    #[test]
    fn binary_version_must_match_exactly() {
        assert!(check_version("gcs-mimic 0.1.0", "0.1.0").is_ok());
        assert!(check_version("gcs-mimic 1.0.0-rc.1", "1.0.0-rc.1").is_ok());
        assert!(check_version("gcs-mimic 1.0.0", "1.0.0-rc.1").is_err());
        assert!(check_version("gcs-mimic 1.0.0-beta.1", "1.0.0-beta.1").is_err());
        assert!(check_version("gcs-mimic 0.2.0", "0.1.0").is_err());
        assert!(check_version("another-tool 0.1.0", "0.1.0").is_err());
    }

    #[test]
    fn invalid_binary_version_preserves_its_parse_error() {
        use std::error::Error;

        let error = check_version("gcs-mimic 1.0.0-beta.1", "1.0.0-beta.1").unwrap_err();
        assert!(error.to_string().contains("1.0.0-beta.1"));
        assert!(
            error
                .source()
                .unwrap()
                .downcast_ref::<InvalidVersion>()
                .is_some()
        );
    }

    #[test]
    fn native_notice_requirements_follow_the_target_runtime() {
        assert!(
            check_native_toolchain("aarch64-unknown-linux-musl", "rustc 1.100.0 (hash)").is_ok()
        );
        assert!(
            check_native_toolchain("aarch64-unknown-linux-musl", "rustc 1.101.0 (hash)").is_err()
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
    fn container_notices_accept_only_static_musl_targets() {
        for target in ["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"] {
            assert!(check_container_target(target).is_ok(), "{target}");
        }
        for target in [
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
        ] {
            assert!(check_container_target(target).is_err(), "{target}");
        }
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
    fn notice_only_output_needs_neither_git_nor_an_executable() {
        let directory = Directory::new();
        for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
            fs::write(directory.0.join(name), "project terms").unwrap();
        }
        let package_dir = directory.0.join("dependency");
        fs::create_dir(&package_dir).unwrap();
        fs::write(package_dir.join("LICENSE"), "dependency terms").unwrap();
        let package = json!({
            "name": "fixture", "version": "1.0.0", "license": "MIT",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "manifest_path": package_dir.join("Cargo.toml"),
        });
        let output = directory.0.join("notices");
        let metadata = json!({"packages": [package], "workspace_members": []});
        write_notices(&directory.0, TARGETS[0], &metadata, &output).unwrap();
        assert_eq!(
            fs::read_to_string(output.join("LICENSE-MIT")).unwrap(),
            "project terms"
        );
        assert_eq!(
            fs::read_to_string(output.join("third-party-licenses/crates/fixture-1.0.0/LICENSE"))
                .unwrap(),
            "dependency terms"
        );
        assert!(
            output
                .join("third-party-licenses/rust/COPYRIGHT-library.html")
                .is_file()
        );
        assert!(output.join("third-party-licenses/rust/licenses").is_dir());
        assert!(
            !output
                .join("third-party-licenses/rust/COPYRIGHT.html")
                .exists()
        );
        let index = fs::read_to_string(output.join("third-party-licenses/INDEX.json")).unwrap();
        assert_eq!(
            fs::read_to_string(output.join("third-party-licenses/mime/NOTICE")).unwrap(),
            include_str!("../../src/mime.NOTICE")
        );
        assert!(index.contains("mime/NOTICE"));
        assert!(!index.contains(directory.0.to_str().unwrap()));
        assert!(!output.join("BUILD-INFO.json").exists());
        assert!(!directory.0.join(".git").exists());
        assert!(!output.join("gcs-mimic").exists());
    }

    #[test]
    fn reviewed_crc32c_sources_are_retained_and_new_versions_require_review() {
        let directory = Directory::new();
        let package_dir = directory.0.join("crc32c");
        fs::create_dir_all(package_dir.join("src")).unwrap();
        for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
            fs::write(package_dir.join(name), format!("crc32c {name}")).unwrap();
        }
        fs::write(package_dir.join("README.md"), "usage notes").unwrap();
        fs::write(
            package_dir.join("src/combine.rs"),
            "zlib attribution and adapted source",
        )
        .unwrap();
        let zlib = directory.0.join(CRC32C_ZLIB_NOTICE);
        fs::create_dir_all(zlib.parent().unwrap()).unwrap();
        fs::write(&zlib, "zlib terms").unwrap();
        // The project's own texts must not replace the crate's license files.
        for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
            fs::write(directory.0.join(name), "project terms").unwrap();
        }
        let mut package = json!({
            "name": "crc32c", "version": CRC32C_REVIEWED_VERSION, "license": "MIT OR Apache-2.0",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "manifest_path": package_dir.join("Cargo.toml"),
        });
        let (_, files) = package_notices(&package).unwrap();
        assert_eq!(
            files,
            BTreeSet::from([
                PathBuf::from("LICENSE-APACHE"),
                PathBuf::from("LICENSE-MIT"),
                PathBuf::from("src/combine.rs"),
            ])
        );
        let output = directory.0.join("notices");
        let metadata = json!({"packages": [package.clone()], "workspace_members": []});
        write_notices(&directory.0, TARGETS[0], &metadata, &output).unwrap();
        let crate_notices = output.join(format!(
            "third-party-licenses/crates/crc32c-{CRC32C_REVIEWED_VERSION}"
        ));
        assert_eq!(
            fs::read_to_string(crate_notices.join("LICENSE-APACHE")).unwrap(),
            "crc32c LICENSE-APACHE"
        );
        assert_eq!(
            fs::read_to_string(crate_notices.join("ZLIB.txt")).unwrap(),
            "zlib terms"
        );
        assert_eq!(
            fs::read_to_string(crate_notices.join("src/combine.rs")).unwrap(),
            "zlib attribution and adapted source"
        );
        package["version"] = json!("0.6.8");
        assert!(package_notices(&package).is_err());
    }

    #[test]
    fn grpc_rust_crates_without_license_text_receive_the_reviewed_upstream_text() {
        let directory = Directory::new();
        for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
            fs::write(directory.0.join(name), "project terms").unwrap();
        }
        let supplied = directory.0.join(GRPC_RUST_LICENSE);
        fs::create_dir_all(supplied.parent().unwrap()).unwrap();
        fs::write(&supplied, "grpc-rust MIT text").unwrap();
        let package_dir = directory.0.join("tonic-prost");
        fs::create_dir(&package_dir).unwrap();
        let mut package = json!({
            "name": "tonic-prost", "version": "0.14.9", "license": "MIT",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "manifest_path": package_dir.join("Cargo.toml"),
        });
        let output = directory.0.join("notices");
        let metadata = json!({"packages": [package.clone()], "workspace_members": []});
        write_notices(&directory.0, TARGETS[0], &metadata, &output).unwrap();
        let crate_notices = output.join("third-party-licenses/crates/tonic-prost-0.14.9");
        assert_eq!(
            fs::read_to_string(crate_notices.join("LICENSE")).unwrap(),
            "grpc-rust MIT text"
        );
        let index: Value = serde_json::from_slice(
            &fs::read(output.join("third-party-licenses/INDEX.json")).unwrap(),
        )
        .unwrap();
        let entry = index["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "tonic-prost")
            .unwrap();
        assert_eq!(
            entry["license_selection"]["upstream_revision"],
            GRPC_RUST_REVISION
        );
        // An unreviewed version, or an unrelated crate without notices, still fails closed.
        package["version"] = json!("0.14.8");
        assert!(package_notices(&package).is_err());
        package["name"] = json!("other");
        package["version"] = json!("0.14.9");
        assert!(package_notices(&package).is_err());
    }
}
