"""Local PDF layout adapter in the pinned converter environment. No network/models."""
import collections
import importlib.metadata
import json
import logging
from pathlib import Path
import re
import sys

VERSION = "0.1.9"
CJK = r"\u2e80-\u9fff\uf900-\ufaff"


def readable_markdown(raw, filter_furniture=True):
    # Preserve raw extraction alongside the reading projection. Only repeated
    # page-edge furniture and whitespace change; never paraphrase content.
    pages = [p for p in raw.split("\f") if p.strip()]
    heads = collections.Counter(p.strip().splitlines()[0].strip() for p in pages)
    repeated = {h for h, n in heads.items() if n >= 3 and n >= len(pages) * .75}
    removed = 0
    result = []
    for page in pages:
        lines = page.strip().splitlines()
        if filter_furniture and lines and lines[0].strip() in repeated:
            lines.pop(0)
            removed += 1
        if filter_furniture and lines and re.fullmatch(r"\s*\d+\s+(?:of|/)\s+\d+\s*", lines[-1]):
            lines.pop()
            removed += 1
        blocks = re.split(r"\n\s*\n", "\n".join(lines))
        for block in blocks:
            block = block.strip()
            if not block:
                continue
            # Do not reflow Markdown tables or code. Ordinary PDF line wraps
            # become spaces (CJK-adjacent whitespace is then removed).
            if not block.startswith(("|", "```", "    ")):
                block = " ".join(line.strip() for line in block.splitlines())
                block = re.sub(rf"(?<=[{CJK}])\s+(?=[{CJK}，。；：！？、])", "", block)
                block = re.sub(rf"(?<=[，。；：！？、])\s+(?=[{CJK}])", "", block)
            result.append(block)
    return "\n\n".join(result) + "\n", removed


def convert(source, directory):
    if importlib.metadata.version("markitdown") != VERSION:
        raise ValueError("converter_version_mismatch")
    # Read corrected glyphs and geometry together, without general converter
    # dispatch (which would discard the geometry needed for paragraph recovery).
    import pdfplumber
    logging.disable(logging.CRITICAL)
    page_texts = []
    layouts = []
    replacements = 0
    with pdfplumber.open(source) as pdf:
        pages = len(pdf.pages)
        if not 0 < pages <= 1000:
            raise ValueError("pdf_page_limit")
        for page in pdf.pages:
            layout = pdf_paragraphs(page)
            text, count = layout["raw"], layout["replacements"]
            if page.images and not text.strip():
                raise ValueError("ocr_required")
            page_texts.append(text)
            layouts.append(layout)
            replacements += count
            page.close()
    raw = "\n\f\n".join(page_texts)
    if not raw.strip():
        raise ValueError("no_extractable_text")
    if "\ufffd" in raw or "(cid:" in raw or "\0" in raw:
        raise ValueError("unmapped_pdf_glyphs")
    projected, joins = join_pdf_pages(layouts)
    # Page-edge evidence exists only in the geometry stage, not this flattened
    # projection. Do not interpret its final body line as a page counter.
    markdown, removed = readable_markdown(projected, filter_furniture=False)
    removed += sum(p["removed"] for p in layouts)
    if not markdown.strip() or len(markdown.encode()) > 16 * 1024 * 1024:
        raise ValueError("invalid_converted_text_size")
    out = Path(directory)
    (out / "raw.md").write_text(raw, encoding="utf-8")
    (out / "document.md").write_text(markdown, encoding="utf-8")
    (out / "engine.json").write_text(json.dumps({
        "engine": "memex/pdf-layout", "engine_version": "1",
        "pdfplumber_version": importlib.metadata.version("pdfplumber"),
        "converter_environment_markitdown_version": VERSION,
        "adapter_version": 3, "pages": pages, "removed_page_edge_lines": removed,
        "actual_text_replacements": replacements,
        "formatting": {"engine": "pdf-line-geometry-v2", "page_continuations": joins,
                       "paragraph_breaks": dict(collections.Counter(r for p in layouts for r in p["breaks"])),
                       "quality": "needs_review", "sampled_model_check": False},
        "ocr": False, "models": False,
        "limitations": ["text-layer-only", "complex-layout-and-tables-require-review"],
    }), encoding="utf-8")


if __name__ == "__main__":
    try:
        convert(sys.argv[1], sys.argv[2])
    except Exception as error:
        # No source text, paths, or library exception payloads in CLI logs.
        allowed = {"converter_version_mismatch", "pdf_page_limit", "ocr_required",
                   "no_extractable_text", "unmapped_pdf_glyphs", "invalid_converted_text_size",
                   "unsupported_pdf_actual_text_scope", "unsupported_pdf_actual_text_nesting",
                   "ambiguous_pdf_actual_text", "unresolved_pdf_actual_text"}
        reason = str(error) if str(error) in allowed else "pdf_conversion_failed"
        print(reason, file=sys.stderr)
        sys.exit(1)
