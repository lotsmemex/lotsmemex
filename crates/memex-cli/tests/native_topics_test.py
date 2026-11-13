import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("topics", Path(__file__).parents[1] / "src/native_import/topics.py")
topics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(topics)


class TopicsTest(unittest.TestCase):
    def test_distinct_topics_cover_each_item_and_keep_source_tags(self):
        rows = []
        for i, text in enumerate(["星系 宇宙 恒星 天文学", "宇宙 恒星 星系 望远镜", "星系 天文学 恒星 宇宙",
                                  "营养 蛋白质 食物 蔬菜", "食物 蔬菜 营养 蛋白质", "营养 食物 蛋白质 蔬菜"]):
            rows.append(dict(item_id=f"item_{i}", revision_id=f"revision_{i}",
                             source_key=str(i), title=text, content=text * 8, tags=["共享标签"]))
        plan, analysis = topics.build(rows, clusters=2)
        sets = [set(a["item_ids"]) for a in analysis["assignments"]]
        self.assertIn({"item_0", "item_1", "item_2"}, sets)
        self.assertIn({"item_3", "item_4", "item_5"}, sets)
        self.assertEqual(sum(map(len, sets)), 6)
        tag = next(n for n in plan["nodes"] if n["name"] == "共享标签")
        self.assertEqual(sum(m["group_id"] == tag["group_id"] for m in plan["memberships"]), 6)
        self.assertEqual(plan["expected_revisions"]["item_5"], "revision_5")
        self.assertTrue(all(n["sensitivity"] == "private" and not n["read_only"] for n in plan["nodes"]))
        self.assertEqual(topics.build(list(reversed(rows)), clusters=2), (plan, analysis))
        other, _ = topics.build(rows, clusters=2, root_name="Another library", namespace="another-library")
        self.assertTrue(set(n["group_id"] for n in plan["nodes"]).isdisjoint(n["group_id"] for n in other["nodes"]))
        if "MEMEX_BIN" in os.environ:
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / "rows.json"
                source.write_text(json.dumps(rows))
                result = subprocess.run([os.environ["MEMEX_BIN"], "import", "plan-topics", "--input", str(source),
                    "--out", str(root / "plan"), "--root-name", "Another library", "--namespace", "another-library",
                    "--python", sys.executable], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads((root / "plan/groups.json").read_text()), other)
                decision = json.loads((root / "plan/decision-input.json").read_text())
                self.assertEqual(sum(c["item_count"] for c in decision["clusters"]), 6)
                self.assertTrue(all("item_ids" not in c and "content" not in c for c in decision["clusters"]))


if __name__ == "__main__":
    unittest.main()
