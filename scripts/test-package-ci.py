"""Distribution boundary tests using disposable synthetic inputs, not a user library."""

import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import plistlib
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile


spec = importlib.util.spec_from_file_location("package_ci", Path(__file__).with_name("package-ci.py"))
pkg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pkg)
TARGET = f"{platform.system().lower()}-{platform.machine().lower()}".replace("darwin-", "macos-").replace("aarch64", "arm64")


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.root_patch = patch.object(pkg, "ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        original = subprocess.check_output

        def metadata_or_node(args, **kwargs):
            if args[:2] == ["git", "rev-parse"]:
                return "a" * 40 + "\n"
            if args[:2] == ["git", "status"]:
                return b""
            return original(args, **kwargs)

        self.git_patch = patch.object(pkg.subprocess, "check_output", side_effect=metadata_or_node)
        self.git_patch.start()
        self.addCleanup(self.git_patch.stop)
        self.write(".agents/USER.md", b"PRIVATE_SENTINEL")
        self.write(".env", b"SECRET_SENTINEL")

    def write(self, name, body):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
        return path

    def test_cli_allowlist_checksum_and_refusal_to_reuse_output_or_symlinks(self):
        binary = self.write("target/release/memex", b"synthetic executable")
        out = self.root / "out"
        pkg.package("cli", TARGET, out)
        archive = next(out.glob("*.tar.gz"))
        with tarfile.open(archive) as contents:
            self.assertEqual(contents.getnames(), ["memex", "BUILD.json"])
            self.assertEqual(contents.getmember("memex").mode, 0o755)
            self.assertEqual(contents.extractfile("memex").read(), binary.read_bytes())
            metadata = json.load(contents.extractfile("BUILD.json"))
            self.assertEqual(metadata["files"], {"memex": hashlib.sha256(binary.read_bytes()).hexdigest()})
        self.assertEqual(archive.with_name(archive.name + ".sha256").read_text().split()[0], hashlib.sha256(archive.read_bytes()).hexdigest())
        with self.assertRaises(FileExistsError):
            pkg.package("cli", TARGET, out)
        binary.unlink()
        binary.symlink_to(self.root / ".env")
        with self.assertRaisesRegex(ValueError, "invalid_package_input"):
            pkg.package("cli", TARGET, self.root / "rejected")
        self.assertFalse((self.root / "rejected").exists())

    def test_clients_only_package_served_files_and_reject_stale_app(self):
        app = "desktop/gpui/target/app/LotsMemeX.app/Contents/"
        plist = self.write(app + "Info.plist", plistlib.dumps({"CFBundleShortVersionString": "0.1.26"}))
        for name in ["MacOS/memex-desktop", "Resources/LotsMemeX.icns", "_CodeSignature/CodeResources"]:
            self.write(app + name, b"synthetic resource")
        self.write("desktop/gpui/Cargo.toml", b'[workspace.package]\nversion = "0.1.26"\n')
        self.write("mobile/web/index.html", b"synthetic page")
        self.write("mobile/web/.env", b"SECRET_SENTINEL")
        self.write("mobile/web/debug-log.txt", b"PRIVATE_SENTINEL")
        self.write("remote/service/mobile-gateway.js", b"exports.mobileRoutes = () => new Map([['/mobile/index.html', {}]]);")
        out = self.root / "clients"
        pkg.package("clients", TARGET, out)
        archive = next(out.glob("*.zip"))
        with zipfile.ZipFile(archive) as contents:
            self.assertEqual(set(contents.namelist()), {
                "LotsMemeX.app/Contents/Info.plist", "LotsMemeX.app/Contents/MacOS/memex-desktop",
                "LotsMemeX.app/Contents/Resources/LotsMemeX.icns", "LotsMemeX.app/Contents/_CodeSignature/CodeResources",
                "mobile/web/index.html", "BUILD.json",
            })
            self.assertEqual(json.loads(contents.read("BUILD.json"))["version"], "0.1.26")
            self.assertEqual((contents.getinfo("LotsMemeX.app/Contents/MacOS/memex-desktop").external_attr >> 16) & 0o777, 0o755)
        plist.write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.1.25"}))
        with self.assertRaisesRegex(ValueError, "stale_app_version"):
            pkg.package("clients", TARGET, self.root / "stale")
        self.assertFalse((self.root / "stale").exists())


if __name__ == "__main__":
    unittest.main()
