"""Read-only freshness check of actual build inputs; never contacts Google Cloud."""

import datetime
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MAX_RESPONSE = 8 * 1024 * 1024


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "gcs-mimic-dependency-check"})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read(MAX_RESPONSE + 1)
    if len(data) > MAX_RESPONSE:
        raise ValueError(f"response exceeds 8 MiB: {url}")
    return data.decode("utf-8")


def stable_version(version):
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        return None
    return tuple(map(int, version.split(".")))


def index_path(name):
    if len(name) == 1:
        return f"1/{name}"
    if len(name) == 2:
        return f"2/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def latest_crate(index):
    releases = [json.loads(line) for line in index.splitlines() if line.strip()]
    candidates = [r for r in releases if not r["yanked"] and stable_version(r["vers"]) is not None]
    if not candidates:
        raise ValueError("no non-yanked stable crate release")
    return max(candidates, key=lambda r: stable_version(r["vers"]))["vers"]


def compare(label, current, latest):
    state = "CURRENT" if current == latest else "UPDATE_AVAILABLE"
    print(f"{label}: {state} current={current} latest={latest}")
    return current != latest


def check(root=ROOT, get=fetch):
    print(f"checked_at_utc={datetime.datetime.now(datetime.UTC).isoformat()}")
    pin = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    upstream = tomllib.loads(get("https://static.rust-lang.org/dist/channel-rust-stable.toml"))
    latest = upstream["pkg"]["rust"]["version"].split()[0]
    if stable_version(latest) is None:
        raise ValueError("stable channel returned an invalid version")
    drift = compare("rust", pin, latest)
    env = dict(os.environ, RUSTUP_TOOLCHAIN=pin)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version=1", "--no-deps", "--locked", "--offline"],
        cwd=root, env=env, text=True,
    ))
    lock = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    direct = sorted({dep["name"] for pkg in metadata["packages"] for dep in pkg["dependencies"]
                     if dep["source"] == "registry+https://github.com/rust-lang/crates.io-index"})
    for name in direct:
        versions = {p["version"] for p in lock if p["name"] == name and p.get("source", "").startswith("registry+")}
        if len(versions) != 1:
            raise ValueError(f"ambiguous direct dependency resolution: {name}: {versions}")
        latest = latest_crate(get(f"https://index.crates.io/{index_path(name)}"))
        drift |= compare(name, versions.pop(), latest)
    status = json.loads((root / "compatibility/dependency-status.json").read_text())
    for action in status["github_actions"]:
        repo = action["repository"]
        release = json.loads(get(f"https://api.github.com/repos/{repo}/releases/latest"))
        tag = release["tag_name"]
        ref = json.loads(get(f"https://api.github.com/repos/{repo}/git/ref/tags/{tag}"))["object"]
        if ref["type"] == "tag":
            ref = json.loads(get(ref["url"]))["object"]
        if ref["type"] != "commit" or not re.fullmatch(r"[a-f0-9]{40}", ref["sha"]):
            raise ValueError(f"invalid action commit: {repo}")
        for workflow in (root / ".github/workflows").glob("*.yaml"):
            for actual in re.findall(rf"uses:\s*{re.escape(repo)}@([^\s]+)", workflow.read_text()):
                if actual != action["sha"]:
                    raise ValueError(f"action status/workflow mismatch: {workflow}")
        drift |= compare(repo, action["sha"], ref["sha"])
    print("Docker and SDK inputs: not present in this tooling-only slice; add detection when introduced.")
    return int(drift)


if __name__ == "__main__":
    try:
        sys.exit(check())
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"CHECK_FAILED: {error}", file=sys.stderr)
        sys.exit(2)
