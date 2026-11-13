"""Offline source preservation. No Library handles, model calls or text logging.

Date headings nominate article boundaries, not proven bibliographic dates.
Every input byte remains in an archived object and in exactly one coverage range.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

POLICY = "date-heading-coverage-v1"
MAX_FILE = 100 * 1024 * 1024
MAX_TOTAL = 512 * 1024 * 1024
MAX_FILES = 4096
DATE = re.compile(r"^\s*[（(]\s*([一二三四五六七八九〇零○]{4}年[^）)]{0,20})[）)]\s*$")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encoded_json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def coverage(raw, encoding, is_text):
    if not is_text:
        return [{"kind": "attachment", "start": 0, "end": len(raw), "sha256": digest(raw)}]
    text = raw.decode(encoding, errors="strict")
    if text.encode(encoding) != raw:
        raise ValueError("source_encoding_not_roundtrippable")
    lines = text.splitlines(keepends=True)
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line.encode(encoding)))
    boundaries = []
    for i, line in enumerate(lines):
        match = DATE.fullmatch(line)
        if not match:
            continue
        title = i - 1
        while title >= 0 and not lines[title].strip():
            title -= 1
        # A date without a preceding title is retained, never silently discarded.
        if title < 0 or DATE.fullmatch(lines[title]):
            continue
        if boundaries and title <= boundaries[-1][1]:
            continue
        boundaries.append((title, i, match.group(1)))
    result = []
    if not boundaries:
        result.append({"kind": "unclassified", "start": 0, "end": len(raw)})
    elif offsets[boundaries[0][0]]:
        result.append({"kind": "unclassified", "start": 0, "end": offsets[boundaries[0][0]]})
    for n, (title, date, raw_date) in enumerate(boundaries):
        end = offsets[boundaries[n + 1][0]] if n + 1 < len(boundaries) else len(raw)
        result.append({
            "kind": "article_candidate", "start": offsets[title], "end": end,
            "title_start": offsets[title], "title_end": offsets[title + 1],
            "date_start": offsets[date], "date_end": offsets[date + 1],
            "body_start": offsets[date + 1],
            "source_heading_date": {"raw": raw_date, "role": "unknown"},
        })
    for part in result:
        part["sha256"] = digest(raw[part["start"]:part["end"]])
    return result


def read_regular(path):
    path = Path(path).absolute()
    directory = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:-1]:
            next_directory = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = next_directory
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    finally:
        os.close(directory)
    with os.fdopen(fd, "rb") as file:
        before = os.fstat(file.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_FILE:
            raise ValueError("invalid_source_file")
        data = file.read(MAX_FILE + 1)
        after = os.fstat(file.fileno())
    stamp = lambda m: (m.st_dev, m.st_ino, m.st_size, m.st_mtime_ns, m.st_ctime_ns)
    if len(data) > MAX_FILE or stamp(before) != stamp(after):
        raise ValueError("source_changed_during_read")
    return data


def write_new(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as file:
        file.write(data)
        file.flush()
        os.fsync(file.fileno())


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def prepare(source, out, encoding):
    source, out = Path(source), Path(out)
    if encoding not in ("utf-8", "gb18030"):
        raise ValueError("unsupported_source_encoding")
    if source.is_symlink() or not source.is_dir():
        raise ValueError("invalid_source_directory")
    source = source.resolve(strict=True)
    out = out.absolute()
    if out.resolve().is_relative_to(source):
        raise ValueError("output_inside_source")
    files = []
    for parent, dirs, names in os.walk(source, followlinks=False):
        for name in dirs + names:
            if (Path(parent) / name).is_symlink():
                raise ValueError("source_symlink")
        files.extend(Path(parent) / name for name in names)
        if len(files) > MAX_FILES:
            raise ValueError("source_file_limit")
    files.sort(key=lambda p: p.relative_to(source).as_posix())
    if not files:
        raise ValueError("empty_source_directory")
    out.mkdir(mode=0o700)  # Existing bundles are never replaced, even incomplete ones.
    objects = out / "objects"
    objects.mkdir(mode=0o700)
    records, total = [], 0
    for path in files:
        raw = read_regular(path)
        total += len(raw)
        if total > MAX_TOTAL:
            raise ValueError("source_total_limit")
        sha = digest(raw)
        target = objects / sha
        if not target.exists():
            write_new(target, raw)
        relative = path.relative_to(source).as_posix()
        is_text = path.suffix.lower() == ".txt"
        records.append({"source_key": digest(relative.encode()), "relative_path": relative,
                        "sha256": sha, "bytes": len(raw), "encoding": encoding if is_text else None,
                        "coverage": coverage(raw, encoding, is_text)})
    manifest = {"schema_version": 1, "policy": POLICY, "sources": records}
    manifest["input_sha256"] = digest(encoded_json(records))
    sync_dir(objects)
    write_new(out / "sources.json", encoded_json(manifest))
    sync_dir(out)
    sync_dir(out.parent)
    return verify(out)


def verify(bundle):
    bundle = Path(bundle)
    if bundle.is_symlink() or (bundle / "objects").is_symlink():
        raise ValueError("source_symlink")
    manifest = json.loads(read_regular(bundle / "sources.json"))
    if manifest["schema_version"] != 1 or manifest["policy"] != POLICY:
        raise ValueError("invalid_source_manifest")
    sources = manifest["sources"]
    if not 0 < len(sources) <= MAX_FILES or digest(encoded_json(sources)) != manifest["input_sha256"]:
        raise ValueError("source_manifest_mismatch")
    total, candidates, unclassified, keys = 0, 0, 0, set()
    for source in sources:
        relative = Path(source["relative_path"])
        sha = source["sha256"]
        if relative.is_absolute() or ".." in relative.parts or not relative.parts or not re.fullmatch(r"[a-f0-9]{64}", sha):
            raise ValueError("invalid_source_locator")
        if source["encoding"] not in (None, "utf-8", "gb18030"):
            raise ValueError("unsupported_source_encoding")
        key = digest(relative.as_posix().encode())
        if key != source["source_key"] or key in keys:
            raise ValueError("source_identity_conflict")
        keys.add(key)
        raw = read_regular(bundle / "objects" / sha)
        if len(raw) != source["bytes"] or digest(raw) != sha:
            raise ValueError("source_object_mismatch")
        expected = coverage(raw, source["encoding"], source["encoding"] is not None)
        if source["coverage"] != expected:
            raise ValueError("source_coverage_mismatch")
        cursor = 0
        restored = hashlib.sha256()
        for part in expected:
            if part["start"] != cursor or not cursor <= part["end"] <= len(raw):
                raise ValueError("source_coverage_gap")
            restored.update(raw[cursor:part["end"]])
            cursor = part["end"]
            candidates += part["kind"] == "article_candidate"
            unclassified += part["kind"] == "unclassified"
        if cursor != len(raw) or restored.hexdigest() != sha:
            raise ValueError("source_restore_mismatch")
        total += len(raw)
        if total > MAX_TOTAL:
            raise ValueError("source_total_limit")
    return {"files": len(sources), "bytes": total, "article_candidates": candidates,
            "unclassified_ranges": unclassified, "input_sha256": manifest["input_sha256"]}


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--source", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--encoding", choices=("utf-8", "gb18030"), required=True)
    v = sub.add_parser("verify")
    v.add_argument("--bundle", required=True)
    args = parser.parse_args()
    try:
        result = prepare(args.source, args.out, args.encoding) if args.command == "prepare" else verify(args.bundle)
    except Exception:
        # Paths, titles and decode excerpts must never reach stdout/stderr.
        print("book_source_operation_failed", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
