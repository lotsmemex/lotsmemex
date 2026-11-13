//! Synthetic book whose English chapters have no published translation but a
//! completed AI chapter translation (`reader_chapter_translation_v1` discovery
//! through `translate_metadata`, text through `translate_chunk` in small chunks),
//! and the `reader_annotations_ai_translation_v1` ledger, as the frozen contract
//! describes: a monotonic ledger version, a view version binding the pages, whole
//! marks stale when any segment's translation changed, and a single exact
//! last-mutation receipt. Development and tests only; all text is invented.

use crate::ai_marks::{AiAnchor, AiAnnotation, AiAnnotationChange, AI_SIDE, AI_SPACE};
use crate::book::{sha256_hex as sha, ReaderRequest, CHUNK_LIMIT};
use crate::Response;
use std::collections::HashMap;

pub const AI_BOOK_INDEX: usize = 31;
pub const AI_BOOK_ITEM: &str = "item_fixture_000031";
pub const AI_BOOK_TITLE: &str = "The Tide Clock (synthetic AI-translated book)";
pub const AI_BOOK_TITLE_ZH: &str = "潮汐钟（合成 AI 译文书）";
/// Largest `translate_chunk` the fixture serves: a chapter takes several chunks.
pub const AI_CHUNK: usize = 512;

/// (key, title, English body, AI translation of generation 0).
const CHAPTERS: [(&str, &str, &str, &str); 3] = [
    ("chapter-1", "Chapter 1: The Tide Clock",
     "## Chapter 1: The Tide Clock\n\nThe clock in the harbour tower counted tides, not hours. When the tide came in, it rang once; when the tide went out, it stopped.\n\nEvery morning the keeper climbed the tower, oiled the gears and wrote the water level into her book. The tide is coming. The tide is coming. She always said it twice, under her breath.\n\nThe children of the town believed that as long as the clock rang, the sea would never cross the breakwater. The grown-ups never said otherwise; on windy nights they only looked up at the tower lamp a little more often.\n\nOne winter the clock fell half a beat behind. The keeper opened its case and found a brass tooth bitten by the salt air. It took her three evenings to file it smooth again.\n\nAfter that she wrote one line on the first page of her book: the clock does not lie, and neither does the tide.\n\n",
     "## 第一章：潮汐钟\n\n港口塔楼里的那座钟数的是潮汐，不是钟点。潮水来了，钟就响一声；潮水退了，钟就停下。\n\n守钟人每天清晨都要爬上塔楼，给齿轮上油，再把潮位记进本子。潮水来了。潮水来了。她总是这样低声念两遍。\n\n镇上的孩子相信，只要钟还在响，海就不会越过防波堤。大人们从不说破，只是在风大的夜里多看几眼塔楼的灯。\n\n有一年冬天，钟慢了半拍。守钟人拆开外壳，发现一枚铜齿被盐雾咬出了缺口。她用了三个晚上，才把它重新锉平。\n\n从那以后，她在本子的扉页上写了一行字：钟不会骗人，潮水也不会。\n\n"),
    ("chapter-2", "Chapter 2: The Night Ferry",
     "## Chapter 2: The Night Ferry\n\nThe last ferry left at midnight. The captain hung a lantern at the bow; the passengers wrapped themselves in blankets and nobody spoke.\n\nThe lighthouse on the far shore flashed every nine seconds. The captain counted the flashes like heartbeats.\n\n",
     "## 第二章：夜渡\n\n最后一班渡船在午夜离港。船长把灯笼挂在船头，乘客们裹着毯子，谁也不说话。\n\n对岸的灯塔每隔九秒闪一次。船长数着闪光，像数着自己的心跳。\n\n"),
    // A transcript whose AI translation renders the speakers' names in Chinese; its
    // recorded block correspondence is one-to-one, every block matched.
    ("chapter-3", "Chapter 3: The Harbour Podcast",
     "## Chapter 3: The Harbour Podcast\n\n**Quinn:** We set a weekly cap on the tower lamp oil from the first winter, because without one nobody thinks about the cost.\n\n**Thorsten:** The keeper's habits from three winters ago may already be wrong. You should not stand there waiting for the tide; it comes while you do something else.\n\nThat is not a clock problem, it is a way-of-working problem. You have to rearrange the whole evening around it.\n\nQuinn: We read the logbook once a week, to see which nights the lamp burned for nothing.\n\n**Thorsten:** A cheap wick has a low price, but it burns out sooner, so the final bill is higher.\n\nIf you count the evenings spent trimming it, the cheap wick is rarely cheap.\n\n**Quinn:** So we stopped comparing prices and started comparing quiet nights.\n\nThorsten: And the quiet nights went up once nobody stood waiting for the tide.\n\n**Quinn:** The logbook also showed which winters the harbour froze early.\n\n**Thorsten:** Those were the winters the bell rang late, because the ice held the tide back.\n\nNobody wrote that down at the time; it only shows up when you read ten winters side by side.\n\n**Quinn:** So now the keeper notes the ice as well as the tide.\n\n**Thorsten:** And the oil cap moves with it, a little higher in the frozen weeks.\n\nQuinn: Which nobody would have thought of if the cap had never existed.\n\n",
     "## 第三章：港口播客\n\n**奎因：** 从第一个冬天起，我们就给塔楼的灯油定了每周的上限，因为没有上限的时候，没人会去想成本。\n\n**托尔斯滕：** 三个冬天前守灯人的习惯，现在可能已经不对了。你不应该站在那里等潮水，它会在你做别的事的时候来。\n\n这不是钟的问题，是工作方式的问题。你得围绕它重新安排整个傍晚。\n\n奎因：我们每周读一次日志，看哪些夜里灯白白烧着。\n\n**托尔斯滕：** 便宜的灯芯单价低，但烧得更快，所以最后的账单更高。\n\n如果把修剪它的那些傍晚也算上，便宜的灯芯很少真的便宜。\n\n**奎因：** 所以我们不再比较价格，而是比较安静的夜晚。\n\n托尔斯滕：大家不再站着干等潮水之后，安静的夜晚就多了。\n\n**奎因：** 日志也记下了港口哪些冬天结冰得早。\n\n**托尔斯滕：** 那些冬天钟都响得晚，因为冰把潮水拦住了。\n\n当时没人把这件事写下来；只有把十个冬天并排读，它才显出来。\n\n**奎因：** 所以现在守钟人除了潮水，也记下冰。\n\n**托尔斯滕：** 灯油的上限也跟着变，结冰的那几周会高一点。\n\n奎因：要是从来没有那个上限，谁也想不到这一点。\n\n"),
];

/// Byte ranges of a chapter's blank-line blocks, separators included.
fn blocks(body: &str) -> Vec<[usize; 2]> {
    let (mut out, mut at) = (Vec::new(), 0);
    while at < body.len() {
        let end = body[at..].find("\n\n").map_or(body.len(), |i| at + i + 2);
        out.push([at, end]);
        at = end;
    }
    out
}
const PREFACE: &str = "## 译者说明\n\n本书正文还没有出版译本；以下各章是 AI 译文的合成样例，用来检验 AI 译文的显示与标记。\n\n";

/// The canonical Markdown of the item: its (Chinese) translation edition.
pub fn canonical_markdown() -> String {
    PREFACE.into()
}

/// The AI translation of `chapter` after `generation` retranslations.
fn ai_text(chapter: &str, generation: u32) -> Option<String> {
    let base = CHAPTERS.iter().find(|c| c.0 == chapter)?.3;
    Some(if generation == 0 { base.into() } else { base.replacen("。", &format!("（第 {generation} 次重译）。"), 1) })
}

#[derive(Debug, Clone)]
struct Mark {
    annotation: AiAnnotation,
    order: u64,
}

#[derive(Debug, Default)]
struct Ledger {
    seq: u64,
    marks: Vec<Mark>,
    /// (expected version, canonical change JSON, result) of the last mutation.
    last: Option<(String, String, serde_json::Value)>,
}

/// Retranslations and AI-mark ledgers of the synthetic items.
#[derive(Debug, Default)]
pub struct AiBookState {
    generation: HashMap<String, u32>,
    ledgers: HashMap<String, Ledger>,
}

fn err(code: &str) -> Response {
    Response { ok: false, reason_code: Some(code.into()), ..Default::default() }
}
fn ok(v: serde_json::Value) -> Response {
    Response { ok: true, reader: Some(v), ..Default::default() }
}

/// Whether `request` is one this module answers for `item_id`'s requests.
pub fn handles(request: &ReaderRequest) -> bool {
    use ReaderRequest as R;
    match request {
        R::AiAnnotationsList { .. } | R::AiAnnotationsEdit { .. } => true,
        R::Toc { item_id, .. } | R::Chunk { item_id, .. } | R::TranslateMetadata { item_id, .. } | R::TranslateChunk { item_id, .. }
        | R::TranslatePreview { item_id, .. } | R::TranslateRequest { item_id, .. } | R::TranslateAlignment { item_id, .. } | R::Alignment { item_id, .. } => item_id == AI_BOOK_ITEM,
        _ => false,
    }
}

impl AiBookState {
    /// A retranslation of `chapter` publishes: another version and hash.
    pub fn retranslate(&mut self, chapter: &str) {
        *self.generation.entry(chapter.into()).or_default() += 1;
    }

    /// (version, text) of the current AI translation of `chapter`.
    fn current(&self, revision: &str, chapter: &str) -> Option<(String, String)> {
        let generation = self.generation.get(chapter).copied().unwrap_or(0);
        let text = ai_text(chapter, generation)?;
        Some((sha(&format!("ai-projection|{AI_BOOK_ITEM}|{revision}|{chapter}|{generation}")), text))
    }

    fn meta(&self, revision: &str, chapter: &str) -> Option<serde_json::Value> {
        let (version, text) = self.current(revision, chapter)?;
        let source = CHAPTERS.iter().find(|c| c.0 == chapter)?.2;
        Some(serde_json::json!({"version": version, "item_id": AI_BOOK_ITEM, "revision_id": revision, "original_chapter": chapter,
            "source_sha256": sha(source), "target_language": "zh-CN", "sha256": sha(&text), "bytes": text.len(), "provenance": "machine_translation",
            "route": {"provider_id": "fixture", "model_id": "synthetic-translator"}}))
    }

    /// Answers `request` (see `handles`). `item` gives an item's revision.
    pub fn handle(&mut self, request: &ReaderRequest, item: &dyn Fn(&str) -> Option<String>, translations: bool, marks: bool) -> Response {
        use ReaderRequest as R;
        match request {
            R::AiAnnotationsList { item_id, offset, limit, expected_version, annotation_space, projection, expected_view_version } => {
                if !marks { return err("invalid_request"); }
                let Some(rev) = item(item_id) else { return err("library_resource_not_found") };
                if annotation_space != AI_SPACE || !(1..=100).contains(limit) || (*offset == 0) != (expected_version.is_none() && expected_view_version.is_none())
                    || expected_version.is_some() != expected_view_version.is_some() {
                    return err("invalid_reader_annotation");
                }
                let ledger = self.ledgers.get(item_id);
                let version = ledger_version(item_id, ledger.map_or(0, |l| l.seq));
                let mut all: Vec<Mark> = ledger.map(|l| l.marks.clone()).unwrap_or_default();
                if let Some(p) = projection {
                    all.retain(|m| m.annotation.anchor.segments.iter().any(|s| s.chapter == p.chapter && s.translation_version == p.translation_version && s.sha256 == p.sha256));
                }
                all.sort_by(|a, b| a.annotation.id.cmp(&b.annotation.id));
                let listed: Vec<serde_json::Value> = all.iter().map(|m| self.listed(&rev, m)).collect();
                let view = sha(&format!("ai-view|{version}|{}|{rev}|{}", serde_json::to_string(projection).unwrap(), serde_json::to_string(&listed).unwrap()));
                if expected_version.as_ref().is_some_and(|v| *v != version) || expected_view_version.as_ref().is_some_and(|v| *v != view) {
                    return err("stale_reader_annotations");
                }
                let page: Vec<_> = listed.iter().skip(*offset).take(*limit).cloned().collect();
                let next = (offset + page.len() < listed.len()).then_some(offset + page.len());
                ok(serde_json::json!({"item_id": item_id, "annotation_space": AI_SPACE, "version": version, "view_version": view,
                    "annotations": page, "total": listed.len(), "next_offset": next}))
            }
            R::AiAnnotationsEdit { item_id, expected_version, annotation_space, change } => {
                if !marks { return err("invalid_request"); }
                let Some(rev) = item(item_id) else { return err("library_resource_not_found") };
                if annotation_space != AI_SPACE { return err("invalid_reader_annotation"); }
                let key = serde_json::to_string(change).unwrap();
                if key.len() > 32 * 1024 { return err("invalid_reader_annotation"); }
                let seq = self.ledgers.get(item_id).map_or(0, |l| l.seq);
                if *expected_version != ledger_version(item_id, seq) {
                    // The exact retry of the last mutation returns its stored result.
                    if let Some((e, c, result)) = self.ledgers.get(item_id).and_then(|l| l.last.as_ref()) {
                        if e == expected_version && *c == key { return ok(result.clone()); }
                    }
                    return err("stale_reader_annotations");
                }
                let (id, outcome) = match change {
                    AiAnnotationChange::Put { annotation } => {
                        let existing = self.ledgers.get(item_id).and_then(|l| l.marks.iter().find(|m| m.annotation.id == annotation.id)).cloned();
                        match existing {
                            Some(m) if m.annotation.anchor != annotation.anchor => return err("reader_annotation_conflict"),
                            Some(m) if !self.is_current(&rev, &m.annotation.anchor) => return err("stale_reader_translation"),
                            Some(m) if m.annotation.style == annotation.style => return err("reader_annotation_conflict"),
                            Some(_) => {
                                let ledger = self.ledgers.entry(item_id.clone()).or_default();
                                let order = ledger.seq + 1;
                                if let Some(m) = ledger.marks.iter_mut().find(|m| m.annotation.id == annotation.id) { m.annotation.style = annotation.style; m.order = order; }
                                (annotation.id.clone(), "style_changed")
                            }
                            None => {
                                // Only this book has AI chapter translations.
                                if item_id != AI_BOOK_ITEM { return err("invalid_reader_anchor"); }
                                if let Some(code) = self.check_new(&rev, &annotation.anchor) { return err(code); }
                                let ledger = self.ledgers.entry(item_id.clone()).or_default();
                                if ledger.marks.iter().any(|m| m.annotation.anchor == annotation.anchor && m.annotation.style == annotation.style) { return err("reader_annotation_conflict"); }
                                if ledger.marks.len() >= 4096 { return err("reader_annotation_limit_exceeded"); }
                                let order = ledger.seq + 1;
                                ledger.marks.push(Mark { annotation: annotation.clone(), order });
                                (annotation.id.clone(), "created")
                            }
                        }
                    }
                    AiAnnotationChange::Delete { id } => {
                        let ledger = self.ledgers.entry(item_id.clone()).or_default();
                        let Some(at) = ledger.marks.iter().position(|m| m.annotation.id == *id) else { return err("reader_annotation_not_found") };
                        ledger.marks.remove(at);
                        (id.clone(), "deleted")
                    }
                };
                let ledger = self.ledgers.entry(item_id.clone()).or_default();
                ledger.seq += 1;
                let result = serde_json::json!({"item_id": item_id, "annotation_space": AI_SPACE, "version": ledger_version(item_id, ledger.seq), "id": id, "outcome": outcome});
                ledger.last = Some((expected_version.clone(), key, result.clone()));
                ok(result)
            }
            _ => {
                let Some(rev) = item(AI_BOOK_ITEM) else { return err("library_resource_not_found") };
                self.book(request, &rev, translations)
            }
        }
    }

    fn book(&self, request: &ReaderRequest, rev: &str, translations: bool) -> Response {
        use ReaderRequest as R;
        let original = || CHAPTERS.iter().map(|c| (c.0, c.1, c.2.to_string())).collect::<Vec<_>>();
        let translation = || vec![("preface", "译者说明", PREFACE.to_string())];
        let edition = |side: &str, language: &str, chapters: &[(&str, &str, String)]| {
            let text: String = chapters.iter().map(|c| c.2.as_str()).collect();
            let mut at = 0;
            let list: Vec<_> = chapters.iter().map(|c| {
                let start = at;
                at += c.2.len();
                serde_json::json!({"key": c.0, "title": c.1, "level": 2, "start": start, "end": at, "counterpart": null})
            }).collect();
            serde_json::json!({"side": side, "language": language, "sha256": sha(&text), "bytes": text.len(), "chapters": list})
        };
        match request {
            R::Toc { revision_id, .. } => {
                if revision_id != rev { return err("stale_item_revision"); }
                ok(serde_json::json!({"item_id": AI_BOOK_ITEM, "revision_id": rev, "canonical_sha256": sha(PREFACE), "default_side": "original", "alignment": "chapter_order",
                    "editions": [edition("original", "en", &original()), edition("translation", "zh", &translation())]}))
            }
            R::Chunk { revision_id, side, chapter, offset, limit, .. } => {
                if revision_id != rev { return err("stale_item_revision"); }
                if !(4..=CHUNK_LIMIT).contains(limit) { return err("invalid_reader_limit"); }
                let chapters = match side.as_str() { "original" => original(), "translation" => translation(), _ => return err("reader_side_not_found") };
                let Some(index) = chapters.iter().position(|c| c.0 == chapter) else { return err("reader_chapter_not_found") };
                let text: String = chapters.iter().map(|c| c.2.as_str()).collect();
                let ch_start: usize = chapters[..index].iter().map(|c| c.2.len()).sum();
                let ch_end = ch_start + chapters[index].2.len();
                let start = ch_start + offset;
                if start > ch_end || !text.is_char_boundary(start) { return err("invalid_reader_offset"); }
                let mut end = (start + limit).min(ch_end);
                while !text.is_char_boundary(end) { end -= 1; }
                ok(serde_json::json!({"item_id": AI_BOOK_ITEM, "revision_id": rev, "side": side, "sha256": sha(&text), "evidence_id": format!("evidence_fixture_ai_book_{side}"),
                    "chapter": chapter, "start": start, "end": end, "next_offset": (end < ch_end).then_some(end - ch_start), "markdown": &text[start..end]}))
            }
            _ if !translations => err("invalid_request"),
            R::TranslateMetadata { revision_id, chapter, .. } => {
                if revision_id != rev { return err("stale_item_revision"); }
                if !CHAPTERS.iter().any(|c| c.0 == chapter) { return err("reader_chapter_not_found"); }
                ok(serde_json::json!({"item_id": AI_BOOK_ITEM, "revision_id": rev, "chapter": chapter, "translation": self.meta(rev, chapter)}))
            }
            R::TranslatePreview { revision_id, chapter, .. } => {
                if revision_id != rev { return err("stale_item_revision"); }
                match self.meta(rev, chapter) {
                    Some(meta) => ok(serde_json::json!({"eligible": true, "translation": meta})),
                    None => err("reader_chapter_not_found"),
                }
            }
            R::TranslateChunk { revision_id, chapter, expected_version, offset, limit, .. } => {
                if revision_id != rev { return err("stale_item_revision"); }
                let (Some(mut meta), Some((version, text))) = (self.meta(rev, chapter), self.current(rev, chapter)) else { return err("reader_chapter_not_found") };
                if *expected_version != version { return err("stale_reader_translation"); }
                if *offset > text.len() || !text.is_char_boundary(*offset) || *limit == 0 { return err("invalid_reader_offset"); }
                let mut end = (offset + (*limit).min(AI_CHUNK)).min(text.len());
                while !text.is_char_boundary(end) { end -= 1; }
                let m = meta.as_object_mut().unwrap();
                m.insert("start".into(), (*offset).into());
                m.insert("end".into(), end.into());
                m.insert("markdown".into(), text[*offset..end].into());
                m.insert("next_offset".into(), (end < text.len()).then_some(end).into());
                ok(meta)
            }
            // The recorded correspondence: generation blocks, one to one.
            R::TranslateAlignment { revision_id, chapter, expected_version, offset, limit, .. } => {
                if revision_id != rev { return err("stale_item_revision"); }
                let (Some(source), Some((version, text))) = (CHAPTERS.iter().find(|c| c.0 == chapter).map(|c| c.2), self.current(rev, chapter)) else { return err("reader_chapter_not_found") };
                if *expected_version != version { return err("stale_reader_translation"); }
                let (bo, bt) = (blocks(source), blocks(&text));
                if bo.len() != bt.len() || !(1..=256).contains(limit) { return err("invalid_reader_alignment_request"); }
                let pairs: Vec<_> = bo.iter().zip(&bt).enumerate().skip(*offset).take(*limit)
                    .map(|(i, (o, t))| serde_json::json!({"o": o, "t": t, "kind": if i == 0 { "heading" } else { "paragraph" }, "relation": "matched"})).collect();
                let next = (offset + pairs.len() < bo.len()).then_some(offset + pairs.len());
                ok(serde_json::json!({"item_id": AI_BOOK_ITEM, "revision_id": rev, "version": version, "original_chapter": chapter, "translation_chapter": null,
                    "original_sha256": sha(source), "translation_sha256": sha(&text), "original_bytes": source.len(), "translation_bytes": text.len(),
                    "status": "aligned", "quality": "not_certified", "basis": "translation_blocks", "offset": offset, "total": bo.len(), "next_offset": next, "pairs": pairs}))
            }
            _ => err("invalid_request"),
        }
    }

    /// Current: the anchor's revision and every segment's version and hash are
    /// the current effective translation of their chapter.
    fn is_current(&self, rev: &str, anchor: &AiAnchor) -> bool {
        anchor.revision_id == rev && anchor.segments.iter().all(|s| self.current(rev, &s.chapter).is_some_and(|(v, t)| v == s.translation_version && sha(&t) == s.sha256))
    }

    fn listed(&self, rev: &str, m: &Mark) -> serde_json::Value {
        let a = &m.annotation;
        let projections: Vec<_> = a.anchor.segments.iter().map(|s| {
            let current = a.anchor.revision_id == rev && self.current(rev, &s.chapter).is_some_and(|(v, t)| v == s.translation_version && sha(&t) == s.sha256);
            serde_json::json!({"chapter": s.chapter, "translation_version": s.translation_version, "sha256": s.sha256, "status": if current { "current" } else { "stale" }})
        }).collect();
        serde_json::json!({"id": a.id, "style": a.style, "anchor": a.anchor, "status": if self.is_current(rev, &a.anchor) { "current" } else { "stale" },
            "order": m.order, "projections": projections})
    }

    fn check_new(&self, rev: &str, a: &AiAnchor) -> Option<&'static str> {
        if a.side != AI_SIDE || a.segments.is_empty() || a.segments.len() > 64 { return Some("invalid_reader_annotation"); }
        if a.revision_id != rev { return Some("stale_item_revision"); }
        let mut last: Option<(usize, usize)> = None;
        for s in &a.segments {
            let Some(index) = CHAPTERS.iter().position(|c| c.0 == s.chapter) else { return Some("invalid_reader_anchor") };
            // Toc order, disjoint.
            if last.is_some_and(|(i, end)| index < i || (index == i && s.start < end)) { return Some("invalid_reader_anchor"); }
            last = Some((index, s.end));
            let Some((version, text)) = self.current(rev, &s.chapter) else { return Some("invalid_reader_anchor") };
            if version != s.translation_version || sha(&text) != s.sha256 { return Some("stale_reader_translation"); }
            if s.start >= s.end || s.end > text.len() || !text.is_char_boundary(s.start) || !text.is_char_boundary(s.end) { return Some("invalid_reader_anchor"); }
        }
        None
    }
}

fn ledger_version(item_id: &str, seq: u64) -> String {
    sha(&format!("ai-ledger|{item_id}|{seq}"))
}
