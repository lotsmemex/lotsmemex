"""Offline thematic organization. Private input and labels stay in local files.

The frozen result is a proposed native-group plan, not a model-generated claim.
No writes to Library here. Applying requires the owner-local Store command.
"""
import hashlib
import json
import logging
import os
from pathlib import Path
import re
import sys


def digest(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True).encode()).hexdigest()


def node(key, name, root, parent=None):
    return dict(group_id=key, name=name, kind="collection" if parent else "user",
                parent_id=parent, root_group_id=root, source=None,
                owner_id="owner_local", sensitivity="private", read_only=False, version=1)


def tokenize(text):
    import jieba
    jieba.setLogLevel(logging.ERROR)
    # URLs/markup/identifiers are not topical words. No data leaves this process.
    text = re.sub(r"https?://\S+|<[^>]*>|[a-z]+_[0-9a-f]{16,}", " ", text.lower())
    stops = set("一个 这个 我们 你们 他们 什么 可以 没有 因为 所以 就是 但是 不是 自己 现在 通过 进行 以及 其中 这些 那些 已经 时候 这样 还是 需要 文章 来源 阅读 全文 原文 微信 关注 更多 点击 图片 链接 笔记 收藏 分享 编辑 作者 今日 今天 表示 认为 相关 发现 研究 the and for with this that from have has are was were you your not can will about into more than they their our which html https http www com org amp jpg png jpeg pdf".split())
    return [w for w in jieba.cut(text) if 2 <= len(w) <= 24 and w not in stops
            and re.fullmatch(r"[a-z\u3400-\u9fff]+", w)]


def build(rows, clusters=None, root_name="印象笔记", namespace="evernote-organized-v1"):
    import numpy as np
    from sklearn.feature_extraction.text import TfidfVectorizer
    from sklearn.cluster import KMeans
    import sklearn
    rows = sorted(rows, key=lambda r: r["item_id"])
    if not rows or len({r["item_id"] for r in rows}) != len(rows):
        raise ValueError("invalid_topic_input")
    texts = [(r["title"] + " ") * 3 + (" ".join(r["tags"]) + " ") * 5 + r["content"] for r in rows]
    vectorizer = TfidfVectorizer(tokenizer=tokenize, token_pattern=None, max_features=30000,
                               sublinear_tf=True, max_df=0.75 if len(rows) > 8 else 1.0)
    matrix = vectorizer.fit_transform(texts)
    features = vectorizer.get_feature_names_out()
    usable = np.asarray(matrix.sum(axis=1)).ravel() > 0
    k = min(clusters or max(2, round(len(rows) ** .5 / 1.5)), int(usable.sum()))
    if not k:
        raise ValueError("no_topic_features")
    model = KMeans(n_clusters=k, random_state=0, n_init=10, max_iter=200).fit(matrix[usable])
    labels = np.full(len(rows), -1, dtype=int)
    labels[usable] = model.labels_
    binding = [{"item_id": r["item_id"], "revision_id": r["revision_id"],
                "input_sha256": digest(r)} for r in rows]
    run = digest({"inputs": binding, "algorithm": "tfidf-kmeans-v1", "k": k})
    root = "group_" + digest(namespace)[:32]
    themes = "group_" + digest([root, "themes", run])[:32]
    tags_root = "group_" + digest([root, "tags"])[:32]
    nodes = [node(root, root_name, root),
             node(themes, "主题分组（本地聚类）", root, root),
             node(tags_root, "原始标签", root, root)]
    memberships = [{"group_id": root, "item_id": r["item_id"]} for r in rows]
    assignments = []
    for label in sorted(set(int(x) for x in labels)):
        indexes = np.where(labels == label)[0]
        if label < 0:
            words = []
            title = "待归类（无有效主题词）"
        else:
            scores = np.asarray(matrix[indexes].mean(axis=0)).ravel()
            words = [str(features[i]) for i in np.argsort(-scores)[:4] if scores[i] > 0]
            title = " / ".join(words) or "待归类"
        group = "group_" + digest([root, run, label])[:32]
        nodes.append(node(group, title, root, themes))
        ids = [rows[int(i)]["item_id"] for i in indexes]
        memberships.extend({"group_id": group, "item_id": i} for i in ids)
        assignments.append(dict(group_id=group, suggested_label=title, keywords=words, item_ids=ids))
    tag_items = {}
    for row in rows:
        for tag in row["tags"]:
            if tag.strip():
                tag_items.setdefault(tag, set()).add(row["item_id"])
    for tag, ids in sorted(tag_items.items()):
        group = "group_" + digest([root, "tag", tag])[:32]
        nodes.append(node(group, tag, root, tags_root))
        memberships.extend({"group_id": group, "item_id": i} for i in sorted(ids))
    plan = dict(request_id="topics-" + digest([root, run]), root_group_id=root,
                nodes=nodes, memberships=memberships,
                expected_revisions={r["item_id"]: r["revision_id"] for r in rows})
    analysis = dict(schema_version=1, run_id=run, algorithm="tfidf-kmeans-v1",
                    sklearn_version=sklearn.__version__, seed=0, n_init=10, clusters=k,
                    input_bindings=binding, assignments=assignments, labels_are_suggestions=True,
                    network=False, models=False, source_tags_preserved=True)
    return plan, analysis


def main(source, output, root_name, namespace):
    os.umask(0o077)
    rows = json.loads(Path(source).read_text())
    plan, analysis = build(rows, root_name=root_name, namespace=namespace)
    out = Path(output)
    out.mkdir(mode=0o700, parents=False, exist_ok=False)
    (out / "groups.json").write_text(json.dumps(plan, ensure_ascii=False))
    (out / "analysis.json").write_text(json.dumps(analysis, ensure_ascii=False))
    # A model can judge cluster labels using compact candidates, without moving
    # bodies or enumerating memberships. This private file is NOT sent anywhere.
    decision = dict(schema_version=1, plan_sha256=digest(plan),
                    clusters=[dict(group_id=a["group_id"], suggested_label=a["suggested_label"],
                                   keywords=a["keywords"], item_count=len(a["item_ids"]))
                              for a in analysis["assignments"]])
    (out / "decision-input.json").write_text(json.dumps(decision, ensure_ascii=False))
    print(json.dumps({"items": len(rows), "topic_groups": len(analysis["assignments"]),
                      "nodes": len(plan["nodes"]), "memberships": len(plan["memberships"])}))


if __name__ == "__main__":
    try:
        main(*sys.argv[1:])
    except Exception:
        print("topic_plan_failed", file=sys.stderr)
        sys.exit(1)
