"""Synthetic public-entry tests: no private fixtures or Library writes."""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "src/book/sources.py"
spec = importlib.util.spec_from_file_location("book_sources", SCRIPT)
sources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sources)


class SourceBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "input"
        self.source.mkdir()
        self.bundle = self.root / "bundle"

    def tearDown(self):
        self.temp.cleanup()

    def test_complete_coverage_variants_and_move_without_originals(self):
        first = "无日期前言\r\n\r\n同名\r\n（一九六〇年）\r\n甲(乙)\r\n同名\r\n（一九六一年五月）\r\n尾声\r\n"
        second = first.replace("甲(乙)", "甲乙")
        originals = {"a.txt": first.encode("gb18030"), "b.txt": second.encode("gb18030"),
                     "extra": b"\x00\xff\x01"}
        for name, raw in originals.items():
            (self.source / name).write_bytes(raw)
        result = sources.prepare(self.source, self.bundle, "gb18030")
        self.assertEqual((result["files"], result["article_candidates"], result["unclassified_ranges"]), (3, 4, 2))
        manifest = json.loads((self.bundle / "sources.json").read_bytes())
        self.assertEqual(len({s["sha256"] for s in manifest["sources"]}), 3)
        for source in manifest["sources"]:
            raw = (self.bundle / "objects" / source["sha256"]).read_bytes()
            restored = b"".join(raw[p["start"]:p["end"]] for p in source["coverage"])
            self.assertEqual(restored, originals[source["relative_path"]])
            if source["relative_path"] == "a.txt":
                prefix, early, late = source["coverage"]
                self.assertEqual(prefix["end"], len("无日期前言\r\n\r\n".encode("gb18030")))
                self.assertEqual(raw[early["body_start"]:early["end"]], "甲(乙)\r\n".encode("gb18030"))
                self.assertEqual(late["source_heading_date"], {"raw": "一九六一年五月", "role": "unknown"})
        moved = self.root / "restored-backup"
        shutil.copytree(self.bundle, moved)
        shutil.rmtree(self.source)
        shutil.rmtree(self.bundle)
        self.assertEqual(sources.verify(moved), result)

    def test_tampered_coverage_and_raw_fail_verification(self):
        (self.source / "a.txt").write_bytes("无日期完整内容".encode("gb18030"))
        sources.prepare(self.source, self.bundle, "gb18030")
        path = self.bundle / "sources.json"
        original = path.read_bytes()
        manifest = json.loads(original)
        manifest["sources"][0]["coverage"][0]["start"] = 1
        manifest["input_sha256"] = sources.digest(sources.encoded_json(manifest["sources"]))
        path.write_bytes(sources.encoded_json(manifest))
        with self.assertRaisesRegex(ValueError, "coverage_mismatch"):
            sources.verify(self.bundle)
        path.write_bytes(original)
        obj = self.bundle / "objects" / manifest["sources"][0]["sha256"]
        obj.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "object_mismatch"):
            sources.verify(self.bundle)

    def test_repeated_prepare_never_overwrites_and_symlinks_rejected(self):
        (self.source / "a.txt").write_text("synthetic", encoding="utf-8")
        first = sources.prepare(self.source, self.bundle, "utf-8")
        with self.assertRaises(FileExistsError):
            sources.prepare(self.source, self.bundle, "utf-8")
        self.assertEqual(sources.verify(self.bundle), first)
        (self.source / "link.txt").symlink_to(self.source / "a.txt")
        with self.assertRaisesRegex(ValueError, "source_symlink"):
            sources.prepare(self.source, self.root / "other", "utf-8")

    def test_invalid_encoding_is_sanitized_at_cli_and_no_complete_manifest(self):
        (self.source / "private-title.txt").write_bytes(b"secret excerpt\xff")
        result = subprocess.run([sys.executable, str(SCRIPT), "prepare", "--source", str(self.source),
                                 "--out", str(self.bundle), "--encoding", "utf-8"], capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(result.stderr, b"book_source_operation_failed\n")
        self.assertFalse((self.bundle / "sources.json").exists())


if __name__ == "__main__":
    unittest.main()
