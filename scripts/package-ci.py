"""Package build candidates, never a checkout, private state or a source archive."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import plistlib
import subprocess
import tarfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def regular_file(relative):
    path = ROOT / relative
    # Do not allow a packaging allowlist to follow links outside reviewed source.
    if path.resolve() != path or not path.is_file():
        raise ValueError("invalid_package_input")
    return path.read_bytes()


def package(kind, target, out):
    actual = f"{platform.system().lower()}-{platform.machine().lower()}"
    actual = actual.replace("darwin-", "macos-").replace("aarch64", "arm64")
    if actual != target:
        raise ValueError("package_platform_mismatch")
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    dirty = bool(subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=normal"], cwd=ROOT
    ).strip())
    files = {}
    if kind == "cli":
        files["memex"] = (regular_file("target/release/memex"), 0o755)
    else:
        app = "desktop/gpui/target/app/LotsMemeX.app/Contents/"
        for name in (
            "Info.plist", "MacOS/memex-desktop", "Resources/LotsMemeX.icns",
            "_CodeSignature/CodeResources",
        ):
            files["LotsMemeX.app/Contents/" + name] = (
                regular_file(app + name), 0o755 if name.startswith("MacOS/") else 0o644
            )
        # Reuse the gateway's actual serving allowlist; never zip mobile/web wholesale.
        script = """
const path = require('node:path');
const {mobileRoutes} = require('./remote/service/mobile-gateway');
const routes = mobileRoutes(path.resolve('mobile/web'));
process.stdout.write(JSON.stringify([...routes.keys()]));
"""
        routes = json.loads(subprocess.check_output(["node", "-e", script], cwd=ROOT))
        for route in routes:
            prefix = "/mobile/"
            if not route.startswith(prefix):
                raise ValueError("invalid_mobile_manifest")
            name = route[len(prefix):]
            if not name or any(p in ("", ".", "..") for p in name.split("/")):
                raise ValueError("invalid_mobile_manifest")
            files["mobile/web/" + name] = (regular_file("mobile/web/" + name), 0o644)
        info = plistlib.loads(files["LotsMemeX.app/Contents/Info.plist"][0])
        version = info["CFBundleShortVersionString"]
        # The desktop suite checks source versions; also reject an old packaged App.
        import tomllib
        workspace = tomllib.loads(regular_file("desktop/gpui/Cargo.toml").decode())
        if version != workspace["workspace"]["package"]["version"]:
            raise ValueError("stale_app_version")
    metadata = {
        "schema_version": 1,
        "kind": kind,
        "commit": commit,
        "source_tree_dirty": dirty,
        "platform": target,
        "distribution": "build_candidate_not_notarized",
        "files": {name: hashlib.sha256(data).hexdigest() for name, (data, _) in files.items()},
    }
    if kind == "clients":
        metadata["version"] = version
    files["BUILD.json"] = ((json.dumps(metadata, indent=2) + "\n").encode(), 0o644)
    # The Actions upload glob is safe only for a newly created output directory.
    out.mkdir(parents=True, exist_ok=False)
    suffix = ".tar.gz" if kind == "cli" else ".zip"
    artifact = out / f"memex-{kind}-{target}-{commit[:12]}{suffix}"
    # A rerun must not accidentally upload an old candidate left in the output directory.
    with artifact.open("xb") as stream:
        if kind == "cli":
            with tarfile.open(fileobj=stream, mode="w:gz") as archive:
                for name, (data, mode) in files.items():
                    entry = tarfile.TarInfo(name)
                    entry.size, entry.mode = len(data), mode
                    archive.addfile(entry, io.BytesIO(data))
        else:
            with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
                for name, (data, mode) in files.items():
                    entry = zipfile.ZipInfo(name)
                    entry.create_system = 3
                    entry.external_attr = (0o100000 | mode) << 16
                    entry.compress_type = zipfile.ZIP_DEFLATED
                    archive.writestr(entry, data)
    checksum = hashlib.sha256(artifact.read_bytes()).hexdigest()
    artifact.with_name(artifact.name + ".sha256").write_text(f"{checksum}  {artifact.name}\n")
    print(artifact.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("cli", "clients"))
    parser.add_argument("--platform", required=True, choices=("linux-x86_64", "macos-arm64"))
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    package(args.kind, args.platform, args.out)
