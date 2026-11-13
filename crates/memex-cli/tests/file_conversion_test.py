"""Real CLI + pinned PDF engine on generated, nonprivate fixtures.

MEMEX_BIN=target/debug/memex MEMEX_PDF_PYTHON=/path/to/venv/bin/python \
  python3 crates/memex-cli/tests/file_conversion_test.py
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("adapter", Path(__file__).parents[1] / "src/file_conversion/convert.py")
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)

geometry_spec = importlib.util.spec_from_file_location("pdf_geometry", Path(__file__).parents[1] / "src/file_conversion/pdf_text.py")
geometry = importlib.util.module_from_spec(geometry_spec)
geometry_spec.loader.exec_module(geometry)


def pdf_fixture(image=False, actual_text=None, scope=None, raw_content=None, second_page=None):
    # Standard Adobe CJK font with a Unicode CMap, plus asymmetric Latin text.
    chinese = "中文正文，标点不能消失。第二段？".encode("utf-16-be").hex()
    content = (f"BT /F1 12 Tf 40 750 Td (First paragraph: 17.5%, not 5.1%.) Tj ET\n"
               f"BT /F2 12 Tf 40 700 Td <{chinese}> Tj ET").encode()
    if image:
        content = b"q 20 0 0 20 40 700 cm /Im1 Do Q"
    if raw_content is not None:
        content = raw_content
    if actual_text is not None:
        content = content.replace(b"BT /F2", b"/P <</MCID 0>> BDC BT /F2") + b"\nEMC"
        if scope == "nested":
            content = content.replace(b"\nEMC", b"\n/Span BMC BT /F1 12 Tf 40 650 Td (nested glyphs) Tj ET EMC\nBT /F1 12 Tf 40 620 Td (tail glyphs) Tj ET\nEMC")
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R /F2 6 0 R >> /XObject << /Im1 8 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream",
        b"<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H /DescendantFonts [7 0 R] >>",
        b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> >>",
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\n\x00\x00\x00\nendstream",
    ]
    if actual_text is not None:
        objects[0] = b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 9 0 R /MarkInfo <</Marked true>> >>"
        objects[2] = objects[2].replace(b"/Parent 2 0 R", b"/Parent 2 0 R /StructParents 0")
        actual = (b"\xfe\xff" + actual_text.encode("utf-16-be")).hex().encode()
        objects.extend([
            b"<< /Type /StructTreeRoot /K [10 0 R] /ParentTree 11 0 R >>",
            b"<< /Type /StructElem /S /P /P 9 0 R /Pg 3 0 R /K 0 /ActualText <" + actual + b"> >>",
            b"<< /Nums [0 [10 0 R]] >>",
        ])
        if scope == "cross-page":
            objects[1] = b"<< /Type /Pages /Count 2 /Kids [3 0 R 12 0 R] >>"
            objects[9] = objects[9].replace(b"/Pg 3 0 R /K 0", b"/K [<< /Type /MCR /Pg 3 0 R /MCID 0 >> << /Type /MCR /Pg 12 0 R /MCID 0 >>]")
            objects[10] = b"<< /Nums [0 [10 0 R] 1 [10 0 R]] >>"
            objects.append(objects[2].replace(b"/StructParents 0", b"/StructParents 1"))
    if second_page is not None:
        assert actual_text is None
        extra = second_page if isinstance(second_page, list) else [second_page]
        kids = [3]
        for content in extra:
            stream_id = len(objects) + 1
            objects.append(b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream")
            objects.append(objects[2].replace(b"/Contents 5 0 R", f"/Contents {stream_id} 0 R".encode()))
            kids.append(len(objects))
        objects[1] = f"<< /Type /Pages /Count {len(kids)} /Kids [{' '.join(f'{i} 0 R' for i in kids)}] >>".encode()
    result = b"%PDF-1.4\n"
    offsets = [0]
    for i, obj in enumerate(objects, 1):
        offsets.append(len(result))
        result += f"{i} 0 obj\n".encode() + obj + b"\nendobj\n"
    start = len(result)
    result += f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode()
    result += b"".join(f"{n:010d} 00000 n \n".encode() for n in offsets[1:])
    return result + f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n".encode()


class ConversionTests(unittest.TestCase):
    def invoke(self, pdf, out):
        return subprocess.run([os.environ["MEMEX_BIN"], "file", "convert", str(pdf),
            "--out", str(out), "--python", os.environ["MEMEX_PDF_PYTHON"]], capture_output=True, text=True)

    def test_boundary_geometry_requires_evidence_not_lowercase(self):
        a = dict(text="unfinished words", size=12, body_size=12, left=100, right=412,
                 x0=100, x1=412, top=690, bottom=702, page_width=612, page_height=792,
                 line_count=4, full_lines=3, ambiguous=False, page_number=1)
        b = dict(a, text="lowercase continuation", top=70, bottom=82, page_number=2)
        def join(left, right):
            return geometry.join_pdf_pages([
                dict(blocks=[left["text"]], first=left, last=left),
                dict(blocks=[right["text"]], first=right, last=right)])
        self.assertEqual(join(a, b), ("unfinished words\nlowercase continuation", 1))
        for field, value in (("page_number", 3), ("x0", 124), ("size", 18),
                             ("ambiguous", True), ("full_lines", 1), ("top", 220),
                             ("text", "# heading"), ("text", "1. note"), ("text", "| table |")):
            with self.subTest(field=field, value=value):
                self.assertEqual(join(a, dict(b, **{field: value}))[1], 0)
        for text in ("End.", "End!", "End?", "End。", 'End.”', "Label:"):
            self.assertEqual(join(dict(a, text=text), b)[1], 0)

    def test_novel_page_edges_and_conservative_negative_controls(self):
        def line(text, x, y, size=12):
            return f"BT /F1 {size} Tf {x} {y} Td ({text}) Tj ET\n".encode()

        # Normal book margins, below the old 65%-page measure gate.
        wide = "word " * 10 + "tail"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cases = ("plain", "furniture", "sentence", "indent", "heading", "list",
                     "table", "columns", "early", "poetry", "body-repeat", "note", "number",
                     "two-pages", "unique-header", "unmarked-note", "text-footer",
                     "parenthesized-note", "bracketed-note", "source-note")
            for case in cases:
                with self.subTest(case=case):
                    streams = []
                    for n in range(2 if case == "two-pages" else 3):
                        end = wide + ("." if case == "sentence" else "")
                        start = "1. listed item" if case == "list" else wide
                        base = 700 if n or case == "early" else 130
                        if case == "poetry":
                            start, end = "a short verse", "another short verse"
                        body = (line(start, 124 if n and case == "indent" else 100, base,
                                     20 if n and case == "heading" else 12)
                                + line(wide if case != "poetry" else "a refrain", 100, base - 18)
                                + line(end, 100, base - 36))
                        if case in ("table", "columns"):
                            body = b"".join(line("left cell", 100, y) + line("right cell", 400, y)
                                            for y in (base, base - 18, base - 36))
                        if case in ("furniture", "body-repeat"):
                            body = line("Running title", 250, 765, 8 if case == "furniture" else 12) + body
                        if case in ("two-pages", "unique-header"):
                            body = line("Running title" + (str(n) if case == "unique-header" else ""), 250, 765, 8) + body
                        if case == "text-footer":
                            body += line("Running footer", 250, 35, 8)
                        if case == "parenthesized-note":
                            body += line(r"\(1\) Values are provisional", 250, 35, 8)
                        if case == "bracketed-note":
                            body += line("[1] Values are provisional", 250, 35, 8)
                        if case == "source-note":
                            body += line("Source: Synthetic records", 250, 35, 8)
                        if case == "note":
                            body += line("1. A repeated note", 100, 35, 8)
                        if case == "unmarked-note":
                            body += line("An unmarked note.", 250, 35, 8)
                        if case == "number":
                            body += line("2026", 290, 35) + line("1 of 3", 100, 20)
                        streams.append(body)
                    pdf = root / f"{case}.pdf"
                    source = pdf_fixture(raw_content=streams[0], second_page=streams[1:])
                    pdf.write_bytes(source)
                    out = root / case
                    result = self.invoke(pdf, out)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    text = (out / "document.md").read_text()
                    raw = (out / "raw.md").read_text()
                    engine = json.loads(result.stdout)["engine"]
                    self.assertEqual(engine["formatting"]["page_continuations"],
                                     1 if case in ("plain", "furniture") else 0)
                    self.assertEqual(engine["removed_page_edge_lines"], 3 if case == "furniture" else 0)
                    if case == "furniture":
                        self.assertNotIn("Running", text)
                        self.assertEqual(raw.count("Running title"), 3)
                        raw = raw.replace("Running title", "")
                    # No non-whitespace edits, reordering, invented or lost text.
                    self.assertEqual("".join(raw.split()), "".join(text.split()))
                    self.assertEqual(pdf.read_bytes(), source)
                    self.assertEqual((out / "original.pdf").read_bytes(), source)

    def test_geometry_paragraphs_heading_footer_and_cross_page_continuation(self):
        def line(text, x, y, size=12):
            return f"BT /F1 {size} Tf {x} {y} Td ({text}) Tj ET\n".encode()
        wide = "x" * 85
        opening = "Indented first paragraph " + "x" * 60
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for indented, early_end in [(False, False), (True, False), (False, True)]:
                base = 710 if early_end else 154
                first = (line("Chapter heading", 40, 750, 20) + line(opening, 64, base)
                         + line("continues here.", 40, base - 18)
                         + line("Second paragraph", 64, base - 36)
                         + line(wide, 40, base - 54) + line("17", 300, 30))
                second = (line("Across page", 64 if indented else 40, 750)
                          + line(wide, 40, 732) + line("Tail.", 40, 714)
                          + line("18", 300, 30))
                pdf = root / "geometry.pdf"
                pdf.write_bytes(pdf_fixture(raw_content=first, second_page=[second,
                    line("Final page.", 40, 750) + line("19", 300, 30)]))
                out = root / f"{indented}-{early_end}"
                result = self.invoke(pdf, out)
                self.assertEqual(result.returncode, 0, result.stderr)
                text = (out / "document.md").read_text()
                self.assertIn("Chapter heading\n\n" + opening + " continues here.\n\nSecond paragraph", text)
                joined = not indented and not early_end
                self.assertIn(wide + (" " if joined else "\n\n") + "Across page", text)
                self.assertNotIn("17", text)
                self.assertNotIn("18", text)
                self.assertIn("\n17", (out / "raw.md").read_text())
                self.assertEqual(json.loads(result.stdout)["engine"]["formatting"]["page_continuations"], int(joined))

            # A publication year is not pagination, and a mixed-font cover is
            # valid even when neither font matches the arithmetic median.
            for name, content, expected in [
                ("year", line("Publication", 40, 750) + line("2026", 290, 30), "2026"),
                ("cover", line("A title", 40, 750, 24) + line("An author", 40, 650, 12), "A title\n\nAn author"),
                ("credits", line("Title", 230, 550, 42) + line("An author", 170, 440, 16)
                    + line("A translator", 170, 412, 16), "Title\n\nAn author\n\nA translator"),
                ("short-prose", line("Ordinary prose continues", 40, 750, 16)
                    + line("on this line.", 40, 722, 16), "Ordinary prose continues on this line."),
            ]:
                pdf.write_bytes(pdf_fixture(raw_content=content))
                result = self.invoke(pdf, root / name)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(expected, (root / name / "document.md").read_text())

    def test_tagged_actual_text_replaces_glyphs_once_without_losing_unmarked_text(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pdf = root / "tagged.pdf"
            replacement = "正确的替代文字：甲乙丙。"
            pdf.write_bytes(pdf_fixture(actual_text=replacement))
            result = self.invoke(pdf, root / "out")
            self.assertEqual(result.returncode, 0, result.stderr)
            text = (root / "out/document.md").read_text()
            self.assertEqual(text.count(replacement), 1)
            self.assertNotIn("中文正文", text)
            self.assertIn("First paragraph: 17.5%, not 5.1%.", text)
            self.assertEqual(json.loads(result.stdout)["engine"]["actual_text_replacements"], 1)
            for scope, reason in [("cross-page", "unsupported_pdf_actual_text_scope"),
                                  ("nested", "unsupported_pdf_actual_text_nesting")]:
                pdf.write_bytes(pdf_fixture(actual_text=replacement, scope=scope))
                rejected = root / scope
                failure = self.invoke(pdf, rejected)
                self.assertNotEqual(failure.returncode, 0)
                self.assertIn(reason, failure.stderr)
                self.assertFalse(rejected.exists())

    def test_real_engine_preserves_chinese_punctuation_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pdf = root / "input.pdf"
            pdf.write_bytes(pdf_fixture())
            out = root / "bundle"
            result = self.invoke(pdf, out)
            self.assertEqual(result.returncode, 0, result.stderr)
            text = (out / "document.md").read_text()
            self.assertIn("First paragraph: 17.5%, not 5.1%.", text)
            self.assertIn("中文正文，标点不能消失。第二段？", text)
            self.assertGreaterEqual(text.count("\n\n"), 1)
            self.assertEqual((out / "original.pdf").read_bytes(), pdf.read_bytes())
            receipt = json.loads(result.stdout)
            self.assertEqual(receipt["markdown_sha256"], hashlib.sha256(text.encode()).hexdigest())
            self.assertFalse(receipt["engine"]["models"])
            self.assertNotEqual(self.invoke(pdf, out).returncode, 0)
            self.assertEqual((out / "document.md").read_text(), text)

            # Exercise the real CLI Store seam, not a forged revision fixture.
            (root / "old.md").write_text("Old synthetic reading text.")
            artifacts = []
            for name, mime in [("old.md", "text/markdown"), ("input.pdf", "application/pdf")]:
                data = (root / name).read_bytes()
                artifacts.append({"schema_version": 1, "locator": name, "media_type": mime,
                    "byte_size": len(data), "sha256": hashlib.sha256(data).hexdigest()})
            (root / "source.json").write_text(json.dumps({"schema_version": 1, "source_id": "synthetic-file",
                "name": "Synthetic", "source_kind": "file", "default_sensitivity": "private"}))
            (root / "capture.json").write_text(json.dumps({"schema_version": 1, "adapter_id": "synthetic-file",
                "adapter_version": "1", "external_capture_id": "one", "source_id": "synthetic-file",
                "captured_at": "2026-01-02T03:04:05Z", "capture_method": "file",
                "sensitivity": "private", "title": "Synthetic PDF", "source_url": "urn:synthetic:pdf",
                "artifacts": artifacts}))
            library = root / "library"
            def cli(*args):
                return subprocess.run([os.environ["MEMEX_BIN"], *map(str, args)], capture_output=True, text=True)
            ingested = cli("ingest", "--source", root / "source.json", "--capture", root / "capture.json", "--library", library)
            self.assertEqual(ingested.returncode, 0, ingested.stderr)
            stored = json.loads(ingested.stdout)
            item = stored["item_ids"][0]
            before = json.loads(cli("show", "--library", library, "--item", item).stdout)
            apply_args = ["file", "apply-conversion", "--bundle", out, "--library", library,
                "--item", item, "--expected-revision-id", stored["revision_ids"][0], "--max-sensitivity", "private"]
            applied = cli(*apply_args)
            self.assertEqual(applied.returncode, 0, applied.stderr)
            after = json.loads(cli("show", "--library", library, "--item", item).stdout)
            self.assertEqual(after["item"]["item_type"], "document")
            self.assertEqual(after["item"]["added_at"], before["item"]["added_at"])
            self.assertEqual(after["current_revision"]["content_markdown"], text)
            self.assertEqual(after["current_revision"]["evidence_ids"][:-1], before["current_revision"]["evidence_ids"])
            self.assertNotEqual(cli(*apply_args).returncode, 0, "stale CAS must reject replay")
            self.assertTrue((library / "items" / item / "revisions" / (stored["revision_ids"][0] + ".json")).exists())

    def test_image_only_pdf_fails_without_creating_a_fake_success_bundle(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pdf = root / "scan.pdf"
            pdf.write_bytes(pdf_fixture(image=True))
            result = self.invoke(pdf, root / "bundle")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("ocr_required", result.stderr)
            self.assertFalse((root / "bundle").exists())

    def test_common_markdown_audit_never_rewrites_or_echoes_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "from-word-or-web.md"
            text = "# Synthetic heading\n\nPRIVATE_SYNTHETIC_SENTINEL\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n" + "甲" * 2201
            source.write_text(text)
            args = [os.environ["MEMEX_BIN"], "file", "audit-format", str(source), "--out", str(root / "audit")]
            result = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("PRIVATE_SYNTHETIC_SENTINEL", result.stdout + result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["input_sha256"], hashlib.sha256(text.encode()).hexdigest())
            self.assertEqual(report["model_state"], "not_requested")
            self.assertEqual(report["quality"], "not_certified")
            self.assertEqual(report["metrics"]["blocks_over_2000_characters"], 1)
            self.assertEqual(source.read_text(), text)
            self.assertFalse((root / "audit/dispatch.json").exists())
            self.assertNotEqual(subprocess.run(args, capture_output=True).returncode, 0)
            self.assertNotEqual(subprocess.run(args + ["--allow-model"], capture_output=True).returncode, 0)
            reference = {"input_sha256": report["input_sha256"], "source_sha256": "a" * 64,
                "samples": [{"id": s["id"], "lines": [{"page": 1, "x0": 40, "top": 70,
                    "size": 12, "text": "PRIVATE_SYNTHETIC_SENTINEL"}]} for s in report["samples"]]}
            for defect in ("wrong-input", "oversized"):
                bad = json.loads(json.dumps(reference))
                if defect == "wrong-input":
                    bad["input_sha256"] = "b" * 64
                else:
                    bad["samples"][0]["lines"][0]["text"] = "甲" * 1366
                (root / "reference.json").write_text(json.dumps(bad))
                failure = subprocess.run([os.environ["MEMEX_BIN"], "file", "audit-format", str(source),
                    "--out", str(root / defect), "--allow-model", "--model", "synthetic",
                    "--database", str(root / "absent.db"), "--reference", str(root / "reference.json")],
                    capture_output=True, text=True)
                self.assertNotEqual(failure.returncode, 0)
                self.assertIn("invalid_format_reference", failure.stderr)
                self.assertNotIn("PRIVATE_SYNTHETIC_SENTINEL", failure.stderr + failure.stdout)
                self.assertFalse((root / defect).exists())

    def test_reflow_preserves_paragraphs_tables_and_latin_word_spaces(self):
        raw = "中文 正文，\n继续。\n\nMixed English\nwords: 17.5%.\n\n| a | b |\n|---|---|\n| 1 | 2 |"
        text, removed = adapter.readable_markdown(raw)
        self.assertEqual(text, "中文正文，继续。\n\nMixed English words: 17.5%.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n")
        self.assertEqual(removed, 0)
        pages = "\f".join(f"Repeated heading\n\nUnique body {n}.\n\n{n} of 3" for n in range(1, 4))
        text, removed = adapter.readable_markdown(pages)
        self.assertEqual(text, "Unique body 1.\n\nUnique body 2.\n\nUnique body 3.\n")
        self.assertEqual(removed, 6)


if __name__ == "__main__":
    unittest.main()
