"""Complete an offline frozen bundle with local Pandoc/Poppler extraction.

Writes a separate final manifest, never overwrites the original attempt, never
fetches linked resources. Run under the same network-denied sandbox as prepare.
"""
import collections
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

from zotero import encoded, freeze, html_markdown, put, sha

DOCX = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
PDF_RETRY = {"unmapped_pdf_glyphs", "conversion_timeout", "pdf_conversion_failed"}


def read(root, descriptor):
    digest = descriptor["sha256"]
    if not re.fullmatch("[0-9a-f]{64}", digest) or descriptor["path"] != "objects/" + digest:
        raise ValueError("invalid_bundle_locator")
    path = root / descriptor["path"]
    if path.is_symlink():
        raise ValueError("invalid_bundle_locator")
    data = path.read_bytes()
    if sha(data) != digest:
        raise ValueError("bundle_hash_mismatch")
    return data


def extract(source, kind):
    executable = shutil.which("pandoc" if kind == "docx" else "pdftotext")
    if not executable:
        raise ValueError("converter_unavailable")
    command = ([executable, "--from=docx", "--to=gfm", "--wrap=none", str(source)] if kind == "docx"
               else [executable, "-enc", "UTF-8", str(source), "-"])
    result = subprocess.run(command, capture_output=True, timeout=120)
    if result.returncode:
        raise ValueError("conversion_failed")
    text = result.stdout.decode("utf-8")
    if not text.strip() or len(result.stdout) > 16 * 1024 * 1024 or any(s in text for s in ("\ufffd", "(cid:", "\0")):
        raise ValueError("invalid_extracted_text")
    if kind == "pdf":
        pages = text.split("\f")
        if not pages[-1].strip():
            pages.pop()
        # Do not call a partial text layer a complete scanned document.
        if any(not p.strip() for p in pages):
            raise ValueError("page_without_text")
        text = "\n\n".join(p.strip() for p in pages) + "\n"
    version = subprocess.run([executable, "--version" if kind == "docx" else "-v"],
                             capture_output=True, timeout=10)
    version_line = (version.stdout or version.stderr).decode(errors="replace").splitlines()[0]
    return text, dict(engine=version_line, adapter_version=1, network=False, models=False,
                      ocr=False, limitations=["complex-layout-and-figures-require-review"])


def supplement(root):
    root = Path(root).resolve()
    if (root / "items-final.json").exists():
        raise ValueError("final_manifest_already_exists")
    plan = json.loads((root / "items-plan.json").read_text())
    hierarchy = json.loads((root / "hierarchy.json").read_text())
    by_key = {row["key"]: row for row in hierarchy["items"]}
    counts, failures = collections.Counter(), collections.Counter()
    for index, descriptor in enumerate(plan["records"]):
        record = json.loads(read(root, descriptor))
        kind = None
        if len(record["evidence"]) >= 2:
            original = record["evidence"][1]
            if record["status"] == "unsupported_format" and original["media_type"] == DOCX:
                kind = "docx"
            elif record["status"] in PDF_RETRY:
                kind = "pdf"
        if kind:
            read(root, original)
            try:
                text, engine = extract(root / original["path"], kind)
            except (ValueError, subprocess.TimeoutExpired, UnicodeError):
                failures[kind] += 1
            else:
                raw = json.loads(read(root, record["evidence"][0]))
                if raw.get("note") and raw["note"]["note"].strip():
                    text += "\n\n## 附件笔记\n\n" + html_markdown(raw["note"]["note"])
                engine.update(previous_outcome=record["status"], source_sha256=original["sha256"])
                record["body"] = put(root, text.encode(), "text/markdown")
                record["evidence"].append(put(root, encoded(engine), "application/json"))
                record["status"] = "converted_docx" if kind == "docx" else "converted_pdf_poppler_text"
                plan["records"][index] = put(root, encoded(record), "application/json")
                by_key[record["key"]]["status"] = record["status"]
        counts[record["status"]] += 1
    freeze(root / "hierarchy-final.json", encoded(hierarchy))
    freeze(root / "preview-final.json", encoded(dict(records=len(plan["records"]), outcomes=dict(counts),
                                                   supplemental_failures=dict(failures), network=False, models=False)))
    # Publish the import plan last; earlier files alone are not ready to import.
    for directory in (root / "objects", root):
        fd = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    freeze(root / "items-final.json", encoded(plan))
    fd = os.open(root, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    print(json.dumps(dict(records=len(plan["records"]), outcomes=dict(counts), supplemental_failures=dict(failures))))


if __name__ == "__main__":
    os.umask(0o077)
    try:
        supplement(sys.argv[1])
    except Exception:
        print("zotero_supplement_failed", file=sys.stderr)
        sys.exit(1)
