"""Offline chapter projection. Existing outlines/explicit numbering only; no model."""
import hashlib
import json
import re
import sys
from pathlib import Path


def reflow(text):
    blocks = []
    for b in re.split(r"\n\s*\n", text.replace("\f", "\n\n")):
        if not b.strip():
            continue
        b = " ".join(line.strip() for line in b.splitlines())
        b = re.sub(r"(?<=[\u2e80-\u9fff，。；：！？、])\s+(?=[\u2e80-\u9fff，。；：！？、])", "", b)
        blocks.append(b)
    return "\n\n".join(blocks) + "\n"


def number(s):
    if s.isascii() and s.isdecimal():
        return int(s)
    digits = {c: i for i, c in enumerate("零一二三四五六七八九")}
    if "十" in s:
        a, b = s.split("十")
        return (digits.get(a, 1) if a else 1) * 10 + digits.get(b, 0)
    return digits.get(s)


def outline_sections(pdf):
    from pdfminer.pdftypes import resolve1
    from pdfminer.pdfdocument import PDFNoOutlines
    try:
        entries = list(pdf.doc.get_outlines())
    except PDFNoOutlines:
        return None
    if not entries:
        return None
    ids = {p.page_obj.pageid: i for i, p in enumerate(pdf.pages)}
    points = []
    for level, title, dest, action, _ in entries:
        if dest is None and action:
            dest = resolve1(action).get("D")
        d = resolve1(dest)
        if isinstance(d, (str, bytes)):
            d = resolve1(pdf.doc.get_dest(d))
        if isinstance(d, dict):
            d = resolve1(d["D"])
        if not isinstance(d, list) or not hasattr(d[0], "objid"):
            raise ValueError("unsupported_outline_destination")
        page = ids[d[0].objid]
        if points and page <= points[-1][0]:
            raise ValueError("overlapping_outline_pages")
        points.append((page, title, min(level, 6)))
    if points[0][0] > 0:
        points.insert(0, (0, "封面与出版信息", 1))
    sections = []
    expected = 1
    for i, (start, title, level) in enumerate(points):
        end = points[i + 1][0] if i + 1 < len(points) else len(pdf.pages)
        pages = []
        for p in pdf.pages[start:end]:
            layout = pdf_paragraphs(p)
            if p.images and not layout["raw"].strip():
                raise ValueError("ocr_required")
            pages.append(layout)
            p.close()
        match = re.fullmatch(r"第([一二三四五六七八九十零0-9]+)章", re.sub(r"\s+", "", title))
        n = number(match[1]) if match else None
        if n is not None:
            if n != expected:
                raise ValueError("nonsequential_chapters")
            expected += 1
        key = f"chapter-{n}" if n else f"extra-{i}"
        # Keep the PDF's actual outline label. The generated heading is a navigation
        # projection, not a claim that this line was present in the text layer.
        text, _ = join_pdf_pages(pages)
        sections.append((key, title, level, "#" * level + " " + title + "\n\n" + reflow(text)))
    return sections


def numbered_sections(raw):
    parts = list(re.finditer(r"(?im)^\s*Part[ \t]+(One|Two|[0-9]+|[IVX]+)[ \t]*$", raw))
    if not parts:
        return [("extra-0", "全文", 1, reflow(raw))]
    sections = []
    if raw[:parts[0].start()].strip():
        sections.append(("extra-0", "Front matter", 1, reflow(raw[:parts[0].start()])))
    ordinal = 0
    for part_ix, part in enumerate(parts):
        end = parts[part_ix + 1].start() if part_ix + 1 < len(parts) else len(raw)
        segment = raw[part.end():end]
        chapters = list(re.finditer(r"(?m)^[ \t]*(\d{1,3})[ \t]*$", segment))
        numbers = [int(m[1]) for m in chapters]
        if not chapters or numbers not in [list(range(1, len(chapters) + 1)), list(range(ordinal + 1, ordinal + len(chapters) + 1))]:
            raise ValueError("nonsequential_chapters")
        for i, ch in enumerate(chapters):
            ordinal += 1
            stop = chapters[i + 1].start() if i + 1 < len(chapters) else len(segment)
            start = 0 if i == 0 else ch.start()
            # All extracted bytes belong to a section, including part headings.
            text = (raw[part.start():part.end()] if i == 0 else "") + segment[start:stop]
            title = f"Part {part_ix + 1} · Chapter {int(ch[1])}"
            sections.append((f"chapter-{ordinal}", title, 2, "## " + title + "\n\n" + reflow(text)))
    return sections


def prepare(root):
    import logging
    import pdfplumber
    logging.disable(logging.CRITICAL)
    editions = []
    for side, language in [("original", sys.argv[2]), ("translation", sys.argv[3])]:
        raw = (root / f"{side}.raw.md").read_text()
        receipt = root / f"{side}.conversion.json"
        if receipt.exists() and json.loads(receipt.read_text())["engine"]["adapter_version"] >= 3:
            # New converters preserve paragraph geometry; do not discard it when
            # the PDF lacks a usable outline. Legacy packages keep their contract.
            raw = (root / f"{side}.converted.md").read_text()
        try:
            with pdfplumber.open(root / f"{side}.pdf") as pdf:
                sections = outline_sections(pdf)
            if sections is None:
                sections = numbered_sections(raw)
        except ValueError as e:
            if str(e) not in {"unsupported_outline_destination", "overlapping_outline_pages", "nonsequential_chapters"}:
                raise
            # A navigation ambiguity is not a failed text extraction. Preserve
            # every extracted byte (apart from whitespace reflow) without links.
            sections = [("extra-0", "全文（目录待整理）", 1, reflow(raw))]
        text = ""
        chapters = []
        offset = 0
        for key, title, level, body in sections:
            size = len(body.encode())
            chapters.append(dict(key=key, title=title, level=level, start=offset, end=offset + size, counterpart=None))
            text += body
            offset += size
        if not text.strip() or len(text.encode()) > 16 * 1024 * 1024 or any(x in text for x in ["\ufffd", "(cid:", "\0"]):
            raise ValueError("invalid_book_text")
        (root / f"{side}.md").write_text(text)
        editions.append(dict(side=side, language=language, sha256=hashlib.sha256(text.encode()).hexdigest(), bytes=offset, chapters=chapters))
    # Explicit numbering in the owner-paired editions supports chapter-level links.
    # No nearest-page/paragraph guessing; different chapter sets remain unaligned.
    keys = [{c["key"] for c in e["chapters"] if c["key"].startswith("chapter-")} for e in editions]
    if keys[0] and keys[0] == keys[1]:
        for e in editions:
            for c in e["chapters"]:
                if c["key"] in keys[0]:
                    c["counterpart"] = c["key"]
    (root / "manifest.json").write_text(json.dumps(dict(schema_version=1, alignment="declared_chapter_order_not_paragraph_alignment", editions=editions), ensure_ascii=False))


if __name__ == "__main__":
    try:
        prepare(Path(sys.argv[1]))
    except Exception as e:
        allowed = {"unsupported_outline_destination", "overlapping_outline_pages", "ocr_required", "nonsequential_chapters", "invalid_book_text"}
        print(str(e) if str(e) in allowed else "book_preparation_failed", file=sys.stderr)
        sys.exit(1)
