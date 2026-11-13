"""Materialize source hierarchy after item receipts exist. No Library writes.

Explicit source memberships and parent-derived reading memberships are recorded
separately; notes/attachments follow their parent's topics without duplicating text.
"""
import hashlib
import json
import os
from pathlib import Path
import sys


def stable_id(value):
    return "group_" + hashlib.sha256(value.encode()).hexdigest()[:32]


def build(hierarchy, receipts):
    group = hierarchy["group"]
    namespace = "zotero:group:" + str(group["groupID"])
    root = stable_id(namespace)
    folders = {r["collectionID"]: r for r in hierarchy["collections"]}
    nodes = [dict(group_id=root, name=group["name"].strip(), kind="zotero_group", parent_id=None,
                  root_group_id=root, source=dict(system="zotero", library_key=namespace, object_key=None),
                  owner_id="owner_local", sensitivity="private", read_only=False, version=1)]
    def folder_id(id):
        return stable_id(namespace + ":collection:" + folders[id]["key"])
    for id, folder in sorted(folders.items()):
        parent = folder["parentCollectionID"]
        nodes.append(dict(group_id=folder_id(id), name=" ".join(folder["collectionName"].split()) or "未命名目录",
                          kind="collection", parent_id=root if parent is None else folder_id(parent),
                          root_group_id=root, source=dict(system="zotero", library_key=namespace, object_key=folder["key"]),
                          owner_id="owner_local", sensitivity="private", read_only=False, version=1))
    mapping = {row["key"]: row for row in receipts}
    items = {row["zotero_key"]: row for row in hierarchy["items"]}
    memberships = set()
    bindings = {}
    derived = []
    excluded = 0
    for row in hierarchy["items"]:
        receipt = mapping[row["key"]]  # incomplete materialization is not success
        if not receipt["visible_revision"]:
            excluded += 1
            continue
        id = receipt["receipt"]["item_ids"][0]
        bindings[id] = receipt["visible_revision"]
        memberships.add((root, id))
        explicit = set(row["collections"])
        for folder in explicit:
            memberships.add((folder_id(folder), id))
        parent = items.get(row["parent_key"])
        if parent:
            for folder in set(parent["collections"]) - explicit:
                memberships.add((folder_id(folder), id))
                derived.append(dict(item_id=id, group_id=folder_id(folder),
                                    source_parent_key=parent["key"], origin="parent_collection_reading_membership"))
    plan = dict(request_id="zotero-native-" + hashlib.sha256(json.dumps(hierarchy, sort_keys=True).encode()).hexdigest(),
                root_group_id=root, nodes=nodes, memberships=[dict(group_id=g, item_id=i) for g, i in sorted(memberships)],
                expected_revisions=bindings)
    return plan, dict(derived_memberships=derived, excluded=excluded, source_hierarchy_unchanged=True)


if __name__ == "__main__":
    try:
        os.umask(0o077)
        hierarchy = json.loads(Path(sys.argv[1]).read_text())
        receipts = [json.loads(line) for line in Path(sys.argv[2]).read_text().splitlines()]
        plan, provenance = build(hierarchy, receipts)
        output = Path(sys.argv[3])
        with output.open("x") as stream:
            json.dump(plan, stream, ensure_ascii=False)
        with output.with_suffix(".provenance.json").open("x") as stream:
            json.dump(provenance, stream, ensure_ascii=False)
        print(json.dumps(dict(nodes=len(plan["nodes"]), items=len(plan["expected_revisions"]),
                              memberships=len(plan["memberships"]), excluded=provenance["excluded"])))
    except Exception:
        print("zotero_groups_plan_failed", file=sys.stderr)
        sys.exit(1)
