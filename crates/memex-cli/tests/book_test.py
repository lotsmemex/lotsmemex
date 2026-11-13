"""Synthetic real-process CLI regression; never touches a user's Library."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from file_conversion_test import pdf_fixture

spec = importlib.util.spec_from_file_location("book", Path(__file__).parents[1] / "src/book.py")
book = importlib.util.module_from_spec(spec)
spec.loader.exec_module(book)


class BookTests(unittest.TestCase):
    def test_part_numbering_preserves_front_matter_short_chapters_and_tail(self):
        raw = "Front matter.\n\nPart One\n1\nAlpha.\n\n2\nBrief.\n\nPart Two\n1\nOmega.\nTail.\n"
        sections = book.numbered_sections(raw)
        self.assertEqual([s[0] for s in sections], ["extra-0", "chapter-1", "chapter-2", "chapter-3"])
        self.assertIn("Brief.", sections[2][3])
        self.assertIn("Omega. Tail.", sections[3][3])
        continued = book.numbered_sections(raw.replace("Part Two\n1", "Part Two\n3"))
        self.assertEqual(continued[-1][0], "chapter-3")
        with self.assertRaisesRegex(ValueError, "nonsequential"):
            book.numbered_sections(raw.replace("2\nBrief", "3\nBrief"))

    def test_real_cli_prepare_import_replay_resume_and_bounded_reads(self):
        def cli(*args, ok=True):
            result = subprocess.run([os.environ["MEMEX_BIN"], *map(str, args)], capture_output=True, text=True)
            if ok:
                self.assertEqual(result.returncode, 0, result.stderr)
                return json.loads(result.stdout)
            self.assertNotEqual(result.returncode, 0)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for side in ["en", "zh"]:
                pdf = root / (side + ".pdf")
                pdf.write_bytes(pdf_fixture().replace(b"17.5%", b"82.9%") if side == "en" else pdf_fixture())
                cli("file", "convert", pdf, "--out", root / side, "--python", os.environ["MEMEX_PDF_PYTHON"])
            bundle = root / "book"
            prepared = cli("book", "prepare", "--original", root / "en", "--translation", root / "zh", "--out", bundle, "--title", "Synthetic paired book", "--python", os.environ["MEMEX_PDF_PYTHON"])
            self.assertEqual([e["paired"] for e in prepared["editions"]], [0, 0])
            library = root / "library"
            imported = cli("book", "import", "--bundle", bundle, "--library", library)
            replay = cli("book", "import", "--bundle", bundle, "--library", library)
            self.assertTrue(replay["idempotent_replay"])
            self.assertEqual(imported["item_ids"], replay["item_ids"])
            self.assertEqual(len(list((library / "items").iterdir())), 1)
            item, revision = imported["item_ids"][0], imported["revision_ids"][0]
            def reader(*args, ok=True):
                return cli("reader", "--library", library, "--max-sensitivity", "private", *args, ok=ok)
            toc = reader("toc", item, "--revision", revision)
            for edition in toc["editions"]:
                collected = ""
                offset = 0
                while True:
                    chunk = reader("chunk", item, "--revision", revision, "--side", edition["side"], "--chapter", "extra-0", "--offset", offset, "--limit", 13)
                    self.assertLessEqual(len(chunk["markdown"].encode()), 13)
                    collected += chunk["markdown"]
                    if chunk["next_offset"] is None:
                        break
                    offset = chunk["next_offset"]
                self.assertEqual(collected, (bundle / (edition["side"] + ".md")).read_text())
            # Explicit same-book refresh uses the revision journal, not ingest:
            # one item remains and both edition contracts survive the new revision.
            updated = cli("book", "import", "--bundle", bundle, "--library", library,
                          "--update-item", item, "--expected-revision", revision)
            cli("book", "import", "--bundle", bundle, "--library", library,
                "--update-item", item, "--expected-revision", revision, ok=False)
            revision = updated["revision_id"]
            refreshed = reader("toc", item, "--revision", revision)
            self.assertEqual(refreshed["editions"], toc["editions"])
            self.assertEqual(len(list((library / "items").iterdir())), 1)
            empty = reader("state", item)
            anchor = dict(revision_id=revision, side="translation", sha256=toc["editions"][1]["sha256"], chapter="extra-0", offset=0)
            state = dict(visit_id="mac-open-1", position=anchor, bookmarks=[dict(id="bookmark-1", label="My mark", anchor=anchor)])
            state_file = root / "state.json"
            state_file.write_text(json.dumps(state))
            saved = reader("save", item, "--expected-version", empty["version"], "--state", state_file)
            self.assertEqual(reader("resume"), [item, state])
            # A new process/device sees the same persisted state and stale CAS fails.
            state["visit_id"] = "mobile-open-2"
            state_file.write_text(json.dumps(state))
            reader("save", item, "--expected-version", empty["version"], "--state", state_file, ok=False)
            reader("save", item, "--expected-version", saved["version"], "--state", state_file)
            cli("reader", "--library", library, "--max-sensitivity", "public", "toc", item, "--revision", revision, ok=False)
            original = bundle / "original.pdf"
            before = original.read_bytes()
            blob = library / "reader-blobs" / __import__("hashlib").sha256(before).hexdigest()
            self.assertEqual(blob.read_bytes(), before)
            # Accidental file replacement must fail before creating any library.
            original.write_bytes(before + b"changed")
            rejected_library = root / "rejected"
            cli("book", "import", "--bundle", bundle, "--library", rejected_library, ok=False)
            self.assertFalse(rejected_library.exists())
            # Even an updated preparation checksum cannot override the original
            # converter's PDF→raw/body provenance relationship.
            receipt_path = bundle / "book.json"
            receipt = json.loads(receipt_path.read_text())
            receipt["files"]["original.pdf"] = __import__("hashlib").sha256(original.read_bytes()).hexdigest()
            receipt_path.write_text(json.dumps(receipt))
            cli("book", "import", "--bundle", bundle, "--library", rejected_library, ok=False)
            self.assertFalse(rejected_library.exists())

    def test_alignment_preview_is_chapter_revision_and_permission_bound(self):
        def cli(*args, ok=True):
            result = subprocess.run([os.environ["MEMEX_BIN"], *map(str, args)], capture_output=True, text=True)
            if ok:
                self.assertEqual(result.returncode, 0, result.stderr)
                return json.loads(result.stdout)
            self.assertNotEqual(result.returncode, 0)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for side in ["en", "zh"]:
                # Different editions, explicit paired chapters, no title/private data.
                lines = ["Part One", "1", f"First {side} passage.", "2", f"Second {side} passage."]
                content = b"".join(f"BT /F1 12 Tf 40 {750-i*80} Td ({line}) Tj ET\n".encode() for i, line in enumerate(lines))
                pdf = root / f"{side}.pdf"
                pdf.write_bytes(pdf_fixture(raw_content=content))
                cli("file", "convert", pdf, "--out", root / side, "--python", os.environ["MEMEX_PDF_PYTHON"])
            bundle, library, out = root / "book", root / "library", root / "samples"
            cli("book", "prepare", "--original", root / "en", "--translation", root / "zh", "--out", bundle, "--title", "Fixture", "--python", os.environ["MEMEX_PDF_PYTHON"])
            receipt = cli("book", "import", "--bundle", bundle, "--library", library)
            item, revision = receipt["item_ids"][0], receipt["revision_ids"][0]
            args = ["book", "align", "--library", library, "--item", item, "--revision", revision, "--chapter", "chapter-2", "--out", out, "--max-sensitivity", "private"]
            plan = cli(*args)
            self.assertEqual(plan["requests_this_run"], 0)
            self.assertFalse(out.exists())
            binding = plan["identity"]["binding"]
            self.assertEqual(binding["revision_id"], revision)
            self.assertEqual(binding["original"]["chapter"], "chapter-2")
            self.assertGreater(binding["original"]["chapter_start"], 0)
            self.assertGreater(binding["translation"]["chapter_start"], 0)
            cli(*args[:-1], "public", ok=False)
            batch_args = ["book", "align-all", "--library", library, "--item", item,
                          "--revision", revision, "--out", out, "--max-sensitivity", "private"]
            preview = cli(*batch_args)
            self.assertEqual(len(preview["tasks"]), 2)
            self.assertEqual(preview["failed_chapters"], 0)
            self.assertFalse(preview["library_written"])
            self.assertFalse(out.exists())
            cli(*batch_args, "--allow-model", "--database", root / "missing.db", "--max-requests", "1", ok=False)
            self.assertFalse(out.exists())
            indexes = []
            for chapter, word in [("chapter-1", "First"), ("chapter-2", "Second")]:
                preview_args = args.copy()
                preview_args[9] = chapter
                sample = cli(*preview_args)
                ranges = {}
                for side in ["original", "translation"]:
                    chunk = cli("reader", "--library", library, "--max-sensitivity", "private",
                                "chunk", item, "--revision", revision, "--side", side, "--chapter", chapter)
                    start = chunk["markdown"].encode().index(word.encode())
                    ranges[side] = [start, start + len(word)]
                indexes.append(dict(schema_version=1, identity=sample["identity"],
                                    alignment="sparse_navigation_not_citations", quality="not_certified",
                                    anchors=[dict(**ranges, basis="model_sample_not_full_span_verification")]))
            index = dict(schema_version=1, item_id=item, revision_id=revision, chapters=indexes)
            index_path = root / "index.json"
            index_path.write_text(json.dumps(index))
            save = ["book", "save-alignment", "--library", library, "--bundle", index_path,
                    "--expected-version", "missing", "--max-sensitivity", "private"]
            receipt = cli(*save)
            self.assertEqual(receipt["paired_chapters"], 2)
            self.assertEqual(receipt["anchors"], 2)
            self.assertEqual(cli(*save), receipt)
            read_index = ["reader", "--library", library, "--max-sensitivity", "private",
                          "alignment", item, "--revision", revision]
            self.assertEqual(cli(*read_index)["alignment"], index)
            self.assertEqual(len(list((library / "items").iterdir())), 1)
            import copy
            for corruption in ["hash", "range", "chapter_order", "missing_chapter", "unknown_field"]:
                bad = copy.deepcopy(index)
                if corruption == "hash": bad["chapters"][0]["identity"]["original_sha256"] = "0" * 64
                elif corruption == "range": bad["chapters"][0]["anchors"][0]["translation"] = [0, 999999]
                elif corruption == "chapter_order": bad["chapters"].reverse()
                elif corruption == "missing_chapter": bad["chapters"].pop()
                else: bad["source_text"] = "SOURCE_ECHO_SENTINEL"
                index_path.write_text(json.dumps(bad))
                cli(*save, ok=False)
                self.assertEqual(cli(*read_index)["alignment"], index)
            changed = copy.deepcopy(index)
            changed["chapters"][0]["anchors"] = []
            index_path.write_text(json.dumps(changed))
            cli(*save, ok=False)  # Correct but changed data needs the actual CAS version.
            updated_save = save.copy()
            updated_save[7] = receipt["version"]
            cli(*updated_save)
            self.assertEqual(cli(*read_index)["alignment"], changed)
            denied = read_index.copy()
            denied[4] = "public"
            cli(*denied, ok=False)
            cli("book", "import", "--bundle", bundle, "--library", library, "--update-item", item, "--expected-revision", revision)
            cli(*args, ok=False)
            cli(*read_index, ok=False)
            self.assertFalse(out.exists())

    def test_ambiguous_navigation_keeps_complete_unpaired_text(self):
        # Exercise orchestration fallback, not a fabricated successful chapter map.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            raw = "Part One\n1\nKeep A.\n17\nKeep page-number text.\n2\nKeep B."
            for side in ["original", "translation"]:
                (root / f"{side}.raw.md").write_text(raw)
            import sys
            # The PDF boundary here only supplies an unsupported outline shape;
            # real PDF extraction/import is covered in the process test above.
            with patch.dict(sys.modules, {"pdfplumber": __import__("unittest.mock", fromlist=["MagicMock"]).MagicMock()}), patch.object(book, "outline_sections", side_effect=ValueError("overlapping_outline_pages")), patch.object(sys, "argv", ["book", str(root), "en", "zh"]):
                book.prepare(root)
            manifest = json.loads((root / "manifest.json").read_text())
            self.assertEqual((root / "original.md").read_text(), book.reflow(raw))
            self.assertTrue(all(c["counterpart"] is None for e in manifest["editions"] for c in e["chapters"]))


if __name__ == "__main__":
    unittest.main()
