"""Run with the file-converter Python environment; all source data is synthetic."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("zotero", Path(__file__).parents[1] / "src/native_import/zotero.py")
zotero = importlib.util.module_from_spec(spec)
spec.loader.exec_module(zotero)
spec = importlib.util.spec_from_file_location("zotero_groups", Path(__file__).parents[1] / "src/native_import/zotero_groups.py")
groups = importlib.util.module_from_spec(spec)
spec.loader.exec_module(groups)
sys.path.insert(0, str(Path(__file__).parents[1] / "src/native_import"))
import zotero_supplement as supplement


class NativeZoteroTest(unittest.TestCase):
    def test_supplement_preserves_source_and_converts_docx_and_pdf(self):
        from file_conversion_test import pdf_fixture
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "objects").mkdir()
            source = root / "fixture.docx"
            subprocess.run(["pandoc", "-f", "markdown", "-t", "docx", "-o", str(source)],
                input="# 小节\n\n正文，17.5%不是5.1%。\n\n| 项目 | 数量 |\n|---|---|\n|苹果|17|\n|梨子|5|\n".encode(),
                check=True, capture_output=True)
            raw = zotero.put(root, zotero.encoded(dict(note=None)), "application/json")
            original = zotero.put(root, source.read_bytes(), supplement.DOCX)
            record = dict(key="fixture", evidence=[raw, original], status="unsupported_format",
                body=zotero.put(root, b"placeholder", "text/markdown"))
            old = zotero.encoded(dict(records=[zotero.put(root, zotero.encoded(record), "application/json")]))
            (root / "items-plan.json").write_bytes(old)
            (root / "hierarchy.json").write_text(json.dumps(dict(items=[dict(key="fixture", status="unsupported_format")])))
            if "MEMEX_BIN" in os.environ:
                completed = subprocess.run([os.environ["MEMEX_BIN"], "import", "supplement-zotero",
                    "--bundle", str(root), "--python", sys.executable], capture_output=True, text=True)
                self.assertEqual(completed.returncode, 0, completed.stderr)
            else:
                supplement.supplement(root)
            final = json.loads((root / "items-final.json").read_text())
            converted = json.loads(supplement.read(root, final["records"][0]))
            text = supplement.read(root, converted["body"]).decode()
            self.assertIn("小节", text)
            self.assertIn("正文，17.5%不是5.1%。", text)
            self.assertIn("苹果", text); self.assertIn("17", text); self.assertIn("梨子", text)
            self.assertEqual(converted["status"], "converted_docx")
            self.assertEqual(converted["evidence"][:2], [raw, original])
            self.assertEqual((root / "items-plan.json").read_bytes(), old)
            pdf = root / "fixture.pdf"
            pdf.write_bytes(pdf_fixture())
            text, engine = supplement.extract(pdf, "pdf")
            self.assertIn("中文正文，标点不能消失。第二段？", text)
            self.assertIn("First paragraph: 17.5%, not 5.1%.", text)
            self.assertFalse(engine["ocr"])
            scan = root / "scan.pdf"
            scan.write_bytes(pdf_fixture(image=True))
            mixed = root / "mixed.pdf"
            subprocess.run(["pdfunite", str(pdf), str(scan), str(mixed)], capture_output=True, check=True)
            with self.assertRaisesRegex(ValueError, "page_without_text"):
                supplement.extract(mixed, "pdf")

    def test_group_scope_note_body_hierarchy_and_replay(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            db = sqlite3.connect(root / "snapshot.sqlite")
            db.executescript("""
                create table groups(groupID, libraryID, name);
                insert into groups values(7,3,'Synthetic health');
                create table items(itemID, itemTypeID, libraryID, key);
                insert into items values(1,1,3,'ARTICLE1'),(2,2,3,'NOTE0001'),(3,1,4,'OTHER001'),(4,1,3,'DELETED1');
                create table itemTypesCombined(itemTypeID,typeName);
                insert into itemTypesCombined values(1,'journalArticle'),(2,'note');
                create table deletedItems(itemID); insert into deletedItems values(4);
                create table collections(collectionID,collectionName,parentCollectionID,libraryID,key);
                insert into collections values(10,'Parent',NULL,3,'PARENT01'),(11,'Empty child',10,3,'CHILD001'),(12,'Second folder',NULL,3,'SECOND01');
                create table collectionItems(collectionID,itemID);
                insert into collectionItems values(10,1),(12,1);
                create table itemData(itemID,fieldID,valueID);
                insert into itemData values(1,1,1),(1,2,2),(1,3,3);
                create table fieldsCombined(fieldID,fieldName);
                insert into fieldsCombined values(1,'title'),(2,'date'),(3,'abstractNote');
                create table itemDataValues(valueID,value);
                insert into itemDataValues values(1,'Synthetic bibliography'),(2,'1999'),(3,'First line.
Second line，标点保留。');
                create table itemCreators(itemID,creatorID,creatorTypeID,orderIndex);
                create table creators(creatorID,firstName,lastName,fieldMode);
                create table creatorTypes(creatorTypeID,creatorType);
                create table tags(tagID,name); create table itemTags(itemID,tagID);
                insert into tags values(1,'alpha  beta'),(2,'alpha beta');
                insert into itemTags values(1,1),(1,2);
                create table itemNotes(itemID,parentItemID,note,title);
                insert into itemNotes values(2,1,'<h2>第一节</h2><p>正文，17.5%不是5.1%。</p><script>secret_script()</script><p>末尾不能丢。</p>','Fixture note');
                create table itemAttachments(itemID,parentItemID,linkMode,contentType,path);
                create table itemRelations(itemID,predicateID,object);
            """)
            db.commit(); db.close()
            args = argparse.Namespace(snapshot=str(root / "snapshot.sqlite"), storage=str(root / "storage"), group=7, out=str(root / "bundle"))
            if "MEMEX_BIN" in os.environ:
                prepared = subprocess.run([os.environ["MEMEX_BIN"], "import", "prepare-zotero", "--snapshot", args.snapshot,
                    "--storage", args.storage, "--group", "7", "--out", args.out, "--python", sys.executable], capture_output=True, text=True)
                self.assertEqual(prepared.returncode, 0, prepared.stderr)
            else:
                zotero.prepare(args)
            bundle = root / "bundle"
            plan = json.loads((bundle / "items-plan.json").read_text())
            self.assertEqual(len(plan["records"]), 2)
            records = [json.loads((bundle / r["path"]).read_text()) for r in plan["records"]]
            self.assertEqual(records[0]["status"], "metadata_only")
            self.assertEqual(records[0]["metadata"]["published_date"]["value"], "1999")
            body = (bundle / records[1]["body"]["path"]).read_text()
            self.assertIn("## 第一节", body)
            self.assertIn("正文，17.5%不是5.1%。", body)
            self.assertIn("末尾不能丢。", body)
            self.assertNotIn("secret_script", body)
            raw = json.loads((bundle / records[1]["evidence"][0]["path"]).read_text())
            self.assertIn("secret_script", raw["note"]["note"], "original Evidence must remain intact")
            hierarchy = json.loads((bundle / "hierarchy.json").read_text())
            self.assertEqual(len(hierarchy["collections"]), 3)
            self.assertEqual(hierarchy["items"][1]["parent_key"], "ARTICLE1")
            self.assertEqual(hierarchy["items"][1]["collections"], [])
            if "MEMEX_BIN" in os.environ:
                command = [os.environ["MEMEX_BIN"], "import", "items", "--plan", str(bundle / "items-plan.json"),
                           "--library", str(root / "Library"), "--receipts", str(root / "receipts.jsonl")]
                # An unpublished temporary intent may survive a killed process.
                (bundle / "intents").mkdir()
                (bundle / "intents/.tmp-interrupted").write_bytes(b'"partial timestamp')
                first = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(first.returncode, 0, first.stderr)
                frozen_times = {p.name: p.read_bytes() for p in (bundle / "intents").glob("*.json")}
                with (root / "receipts.jsonl").open("ab") as out:
                    out.write(b'{"key":"interrupted')
                second = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(second.returncode, 0, second.stderr)
                self.assertEqual({p.name: p.read_bytes() for p in (bundle / "intents").glob("*.json")}, frozen_times)
                receipts = [json.loads(line) for line in (root / "receipts.jsonl").read_text().splitlines()]
                self.assertEqual(receipts[0]["receipt"]["item_ids"], receipts[2]["receipt"]["item_ids"])
                self.assertTrue(receipts[3]["receipt"]["idempotent_replay"])
                item = receipts[1]["receipt"]["item_ids"][0]
                revision = receipts[1]["receipt"]["revision_ids"][0]
                stored = json.loads((root / "Library/items" / item / "revisions" / (revision + ".json")).read_text())
                self.assertEqual(stored["content_markdown"], body)
                header = json.loads((root / "Library/items" / item / "item.json").read_text())
                self.assertFalse(header["added_at"].startswith("1999"))
                article_id = receipts[0]["receipt"]["item_ids"][0]
                article_revision = receipts[0]["receipt"]["revision_ids"][0]
                article = json.loads((root / "Library/items" / article_id / "revisions" / (article_revision + ".json")).read_text())
                self.assertEqual(article["metadata"]["abstract_text"]["value"], "First line. Second line，标点保留。")
                self.assertEqual(article["metadata"]["manual_tags"]["value"], ["alpha beta"])
                original = json.loads((bundle / records[0]["evidence"][0]["path"]).read_text())
                self.assertEqual(original["fields"]["abstractNote"], "First line.\nSecond line，标点保留。")
                group_plan, provenance = groups.build(hierarchy, receipts)
                parent = next(n for n in group_plan["nodes"] if n["name"] == "Parent")
                child = next(n for n in group_plan["nodes"] if n["name"] == "Empty child")
                second_folder = next(n for n in group_plan["nodes"] if n["name"] == "Second folder")
                self.assertEqual(child["parent_id"], parent["group_id"])
                self.assertEqual(len(group_plan["nodes"]), 4)
                self.assertEqual(len(group_plan["memberships"]), 6)
                self.assertEqual(len(list((root / "Library/items").iterdir())), 2)
                for folder in (parent, second_folder):
                    self.assertIn(dict(item_id=article_id, group_id=folder["group_id"]), group_plan["memberships"])
                self.assertCountEqual(provenance["derived_memberships"], [dict(item_id=item, group_id=folder["group_id"],
                    source_parent_key="zotero:group:7:item:ARTICLE1", origin="parent_collection_reading_membership")
                    for folder in (parent, second_folder)])
                with self.assertRaises(KeyError):
                    groups.build(hierarchy, receipts[:1])
                hidden = [dict(row, visible_revision=None) if row["key"].endswith("NOTE0001") else row for row in receipts]
                reduced, excluded = groups.build(hierarchy, hidden)
                self.assertEqual(excluded["excluded"], 1)
                self.assertEqual(len(reduced["memberships"]), 3)
                group_path = root / "groups.json"
                planned = subprocess.run([os.environ["MEMEX_BIN"], "import", "plan-zotero-groups",
                    "--hierarchy", str(bundle / "hierarchy.json"), "--receipts", str(root / "receipts.jsonl"),
                    "--out", str(group_path), "--python", sys.executable], capture_output=True, text=True)
                self.assertEqual(planned.returncode, 0, planned.stderr)
                self.assertEqual(json.loads(group_path.read_text()), group_plan)
                applied = subprocess.run([os.environ["MEMEX_BIN"], "import", "groups", "--plan", str(group_path),
                    "--library", str(root / "Library")], capture_output=True, text=True)
                self.assertEqual(applied.returncode, 0, applied.stderr)
                receipt_path = root / "receipts.jsonl"
                good_receipts = receipt_path.read_bytes()
                corrupt = good_receipts + b'{"invalid_completed_record":\n'
                receipt_path.write_bytes(corrupt)
                refused = subprocess.run(command, capture_output=True, text=True)
                self.assertNotEqual(refused.returncode, 0)
                self.assertIn("invalid_completed_import_receipt", refused.stderr)
                self.assertEqual(receipt_path.read_bytes(), corrupt)
                receipt_path.write_bytes(good_receipts)
                (bundle / records[1]["body"]["path"]).write_text("tampered")
                self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)


if __name__ == "__main__":
    unittest.main()
