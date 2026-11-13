"""Prepare a frozen, offline, group-scoped Zotero bundle. Never writes Library.

Run in the pinned file-converter Python environment, with network denied.
The database must be a consistent SQLite backup, never a hot file copy.
"""
import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True).encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def freeze(path, data):
    if path.exists():
        if path.read_bytes() != data:
            raise ValueError("frozen_input_conflict")
        return
    # A crash may leave an unpublished temporary file, never a torn object or
    # manifest under its final name. No overwrite, including concurrent retry.
    with tempfile.NamedTemporaryFile(dir=path.parent) as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
        try:
            os.link(stream.name, path)
        except FileExistsError:
            if path.read_bytes() != data:
                raise ValueError("frozen_input_conflict")


def put(root, data, mime):
    digest = sha(data)
    target = root / "objects" / digest
    if target.exists():
        if file_sha(target) != digest:
            raise ValueError("bundle_hash_conflict")
    else:
        freeze(target, data)
    return dict(path="objects/" + digest, sha256=digest, media_type=mime)


def html_markdown(html):
    from bs4 import BeautifulSoup
    from markdownify import markdownify
    soup = BeautifulSoup(html, "html.parser")
    for element in soup.select("script,style,iframe,object,embed,form,input,button"):
        element.decompose()
    for element in soup.find_all(True):
        for name in list(element.attrs):
            if name.lower().startswith("on"):
                del element.attrs[name]
        for name in ("href", "src"):
            if str(element.get(name, "")).strip().lower().startswith(("javascript:", "data:text/html")):
                del element.attrs[name]
    return markdownify(str(soup), heading_style="ATX").strip() + "\n"


def metadata(fields, creators, tags):
    provenance = dict(origin="intake_provided", provider_id="memex.zotero-native-import", provider_version="1")
    result = {}
    warnings = []
    def add(name, value, limit=4096):
        if value and len(str(value).encode()) <= limit:
            result[name] = dict(value=value, provenance=provenance)
        elif value:
            warnings.append(name + "_retained_in_raw")
    for source, target in {"date": "published_date", "language": "language", "publicationTitle": "journal_title",
                           "publisher": "publisher", "volume": "volume", "issue": "issue", "pages": "pages",
                           "place": "publication_place", "edition": "edition", "series": "series_title",
                           "seriesNumber": "series_number"}.items():
        add(target, fields.get(source, "").strip())
    add("abstract_text", fields.get("abstractNote", "").strip(), 65536)
    if tags and len(tags) <= 256 and all(len(t.encode()) <= 4096 for t in tags):
        add("manual_tags", sorted(set(tags)), 2**24)
    elif tags:
        warnings.append("tags_retained_in_raw")
    names = []
    for c in creators:
        first, last = c["firstName"].strip(), c["lastName"].strip()
        if not (first or last):
            continue
        value = dict(display_name=" ".join(filter(None, [first, last])),
                     role=c["creatorType"] if c["creatorType"] in ["author", "editor", "translator"] else "contributor")
        if c["fieldMode"]:
            value.update(name_type="organization", literal_name=value["display_name"])
        else:
            value.update(name_type="person", family_name=last or None, given_name=first or None)
        names.append(value)
    if len(names) <= 64:
        add("creators", names, 2**24)
    else:
        warnings.append("creators_retained_in_raw")
    identifiers = [dict(kind=kind, value=fields[field].strip()) for field, kind in
                   [("DOI", "doi"), ("ISBN", "isbn"), ("ISSN", "issn")] if fields.get(field, "").strip()]
    add("identifiers", identifiers, 2**24)
    return result, warnings


def local_attachment(storage, key, path):
    if not path or not path.startswith("storage:"):
        return None
    root = storage / key
    relative = Path(path[8:])
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("unsafe_attachment_path")
    current = root
    for part in relative.parts:
        current = current / part
        if current.is_symlink():
            raise ValueError("symlink_attachment")
    if root.is_symlink():
        raise ValueError("symlink_attachment")
    return current if current.is_file() else None


def prepare(args):
    os.umask(0o077)
    snapshot = Path(args.snapshot).resolve()
    storage = Path(args.storage).resolve()
    out = Path(args.out).resolve()
    out.mkdir(mode=0o700, exist_ok=True)
    (out / "objects").mkdir(exist_ok=True)
    (out / "conversions").mkdir(exist_ok=True)
    (out / "attachment-bindings").mkdir(exist_ok=True)
    db = sqlite3.connect(snapshot.as_uri() + "?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    db.execute("PRAGMA query_only=ON")
    group = db.execute("select groupID,libraryID,name from groups where groupID=?", (args.group,)).fetchone()
    if group is None:
        raise ValueError("group_not_found")
    library = group["libraryID"]
    identity = dict(group_id=args.group, source_snapshot_sha256=file_sha(snapshot), adapter_version=1)
    frozen = out / "source-fingerprint.json"
    if frozen.exists() and json.loads(frozen.read_text()) != identity:
        raise ValueError("snapshot_conflict")
    if not frozen.exists():
        freeze(frozen, encoded(identity))
    raw_items = [dict(r) for r in db.execute("select i.*, t.typeName from items i join itemTypesCombined t using(itemTypeID) where libraryID=? and itemID not in(select itemID from deletedItems) order by itemID", (library,))]
    ids = {r["itemID"]: r["key"] for r in raw_items}
    collections_rows = [dict(r) for r in db.execute("select * from collections where libraryID=? order by collectionID", (library,))]
    collection_ids = {r["collectionID"] for r in collections_rows}
    for row in collections_rows:
        if row["parentCollectionID"] is not None and row["parentCollectionID"] not in collection_ids:
            raise ValueError("missing_collection_parent")
    manifest = dict(schema_version=1, source=dict(schema_version=1,
        source_id=f"source_zotero_group_{args.group}", name=group["name"], source_kind="zotero_library",
        canonical_uris=[], evidence_roles=["primary"], default_sensitivity="private", preferred_translator=None), records=[])
    hierarchy = dict(group=dict(group), collections=collections_rows, items=[])
    counts = collections.Counter()
    for index, raw in enumerate(raw_items):
        item_id, key = raw["itemID"], raw["key"]
        fields = dict(db.execute("select f.fieldName,v.value from itemData d join fieldsCombined f using(fieldID) join itemDataValues v using(valueID) where itemID=?", (item_id,)))
        creators = [dict(r) for r in db.execute("select c.*,t.creatorType from itemCreators i join creators c using(creatorID) join creatorTypes t using(creatorTypeID) where itemID=? order by orderIndex", (item_id,))]
        tags = sorted({r[0] for r in db.execute("select name from itemTags join tags using(tagID) where itemID=?", (item_id,)) if r[0].strip()})
        folders = [r[0] for r in db.execute("select collectionID from collectionItems where itemID=?", (item_id,))]
        note = db.execute("select * from itemNotes where itemID=?", (item_id,)).fetchone()
        attachment = db.execute("select * from itemAttachments where itemID=?", (item_id,)).fetchone()
        relations = [dict(r) for r in db.execute("select * from itemRelations where itemID=?", (item_id,))]
        raw.update(fields=fields, creators=creators, tags=tags, collections=folders,
                   note=dict(note) if note else None, attachment=dict(attachment) if attachment else None, relations=relations)
        title = fields.get("title") or (note["title"] if note else "") or "未命名条目"
        typ = re.sub(r"(?<!^)(?=[A-Z])", "_", raw["typeName"]).lower()
        meta, warnings = metadata(fields, creators, tags)
        parent = attachment["parentItemID"] if attachment else note["parentItemID"] if note else None
        native_key = f"zotero:group:{args.group}:item:{key}"
        source_url = fields.get("url", "").strip() or f"urn:zotero:group:{args.group}:item:{key}"
        evidence = [put(out, encoded(raw), "application/json")]
        status = "metadata_only"
        body = f"# {title}\n\n> 题录：本地未提供该条目的正文；关联笔记和附件分别导入。\n\n"
        body += "\n\n".join(f"**{name}**：{value}" for name, value in sorted(fields.items()))
        if typ == "note":
            body = html_markdown(note["note"] if note else "")
            status = "converted_note" if body.strip() else "empty_note"
            body = body if body.strip() else "> 原笔记为空。\n"
        if attachment:
            typ = "document"
            path = local_attachment(storage, key, attachment["path"])
            status = "linked_resource" if attachment["linkMode"] == 3 else "missing_source"
            if path:
                data = path.read_bytes()
                mime = attachment["contentType"] or "application/octet-stream"
                evidence.append(put(out, data, mime))
                freeze(out / "attachment-bindings" / (key + ".json"), encoded(evidence[-1]))
                frozen_source = out / evidence[-1]["path"]
                status = "unsupported_format"
                if mime in ("text/markdown", "text/plain", "text/tab-separated-values"):
                    body = data.decode("utf-8-sig")
                    status = "converted_text"
                elif mime == "text/html":
                    body = html_markdown(data.decode("utf-8-sig"))
                    resources = []
                    for resource in sorted((storage / key).rglob("*")):
                        if resource.is_symlink():
                            raise ValueError("symlink_html_resource")
                        if resource.is_file() and resource != path:
                            blob = put(out, resource.read_bytes(), "application/octet-stream")
                            evidence.append(blob)
                            resources.append(dict(locator=str(resource.relative_to(storage / key)), blob=blob))
                    if resources:
                        evidence.append(put(out, encoded(resources), "application/json"))
                    status = "converted_html"
                elif data.startswith(b"%PDF-"):
                    converted = out / "conversions" / key
                    engine = converted / "engine.json"
                    if not engine.exists():
                        converted.mkdir(exist_ok=True)
                        adapter = Path(__file__).parents[1] / "file_conversion/convert.py"
                        try:
                            process = subprocess.run([sys.executable, str(adapter), str(frozen_source), str(converted)],
                                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=120)
                            reason = process.stderr.decode(errors="replace").strip()
                            status = reason if reason in {"ocr_required", "no_extractable_text", "unmapped_pdf_glyphs", "pdf_page_limit"} else "pdf_conversion_failed"
                        except subprocess.TimeoutExpired:
                            status = "conversion_timeout"
                    if engine.exists():
                        body = (converted / "document.md").read_text()
                        evidence.extend([put(out, (converted / "raw.md").read_bytes(), "text/markdown"),
                                         put(out, engine.read_bytes(), "application/json")])
                        status = "converted_pdf_text"
                elif mime.startswith("image/"):
                    status = "image_original_preserved"
            if not status.startswith("converted_"):
                body = f"# {title}\n\n> 附件正文尚未转换：{status}。原有来源记录和可用的原文件已保留。\n"
            if note and note["note"].strip():
                body += "\n\n## 附件笔记\n\n" + html_markdown(note["note"])
        if not body.strip():
            status, body = "empty_source", "> 来源没有可提取文字。\n"
        record = dict(key=native_key, title=title, item_type=typ, source_url=source_url,
                      metadata=meta, body=put(out, body.encode(), "text/markdown"), evidence=evidence, status=status)
        manifest["records"].append(put(out, encoded(record), "application/json"))
        hierarchy["items"].append(dict(key=native_key, zotero_key=key, collections=folders,
            parent_key=ids.get(parent), parent_item_id=parent, status=status, metadata_warnings=warnings))
        counts[status] += 1
        if (index + 1) % 250 == 0:
            print(json.dumps(dict(processed=index + 1, total=len(raw_items), outcomes=dict(counts))), flush=True)
    freeze(out / "hierarchy.json", encoded(hierarchy))
    freeze(out / "preview.json", encoded(dict(identity=identity, records=len(raw_items), collections=len(collections_rows), outcomes=dict(counts), network=False, models=False)))
    for directory in (out / "objects", out / "attachment-bindings", out):
        descriptor = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    freeze(out / "items-plan.json", encoded(manifest))
    descriptor = os.open(out, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    print(json.dumps(dict(records=len(raw_items), outcomes=dict(counts))), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--snapshot", required=True)
    parser.add_argument("--storage", required=True)
    parser.add_argument("--group", required=True, type=int)
    parser.add_argument("--out", required=True)
    try:
        prepare(parser.parse_args())
    except Exception:
        print("zotero_prepare_failed", file=sys.stderr)
        sys.exit(1)
