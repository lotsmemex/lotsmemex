//! Five-style marks on an AI chapter translation (`reader_annotations_ai_translation_v1`).
//! They live in a ledger of their own (`annotation_space: "ai_translation"`),
//! anchored to byte ranges of the chapter's complete AI translation at its
//! effective version and hash. A selection maps through the renderer's source
//! map (`selection::source_bytes`), never through a text search. A mark whose
//! translation changed is stale as a whole: drawn nowhere, shown as
//! 「译文已更新，标记未迁移」 and only deletable.
use super::*;
use crate::selection::SelectedPart;
use memex_client::ai_marks::{ai_ranges, ai_segment, AiAnchor, AiAnnotation, AiAnnotationChange, AiLedger, ListedAiAnnotation, ProjectionStatus, AI_SIDE};
use memex_client::book::TranslationMeta;
use memex_client::reader_state::{EditOutcome, MarkStyle};

/// The AI-mark edit sent last whose outcome is unknown: kept exactly as sent
/// (same expected version, same change) until a read shows it or the user
/// sends exactly it again.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingAiMark {
    pub item_id: String,
    pub expected: String,
    pub change: AiAnnotationChange,
    pub id: String,
}

#[derive(Default)]
pub(super) struct AiMarks {
    pub supported: bool,
    /// item|revision the ledger belongs to.
    pub loaded_for: Option<String>,
    pub ledger: Option<AiLedger>,
    pub pending: Option<PendingAiMark>,
    pub busy: bool,
    pub notice: Option<String>,
    /// Stale marks still to delete after one 删除旧标记 click (in order).
    pub deleting: Vec<String>,
    pub ticket: u64,
}

/// What a selection in an AI translation can be marked as: one byte range of
/// the chapter's complete AI translation (`text_len` bytes), or why not.
pub(crate) fn ai_target<'a>(parts: impl IntoIterator<Item = &'a SelectedPart>, supported: bool, text_len: usize) -> Result<Range<usize>, &'static str> {
    if !supported { return Err("译文暂不支持保存标记"); }
    let mut range: Option<Range<usize>> = None;
    for p in parts {
        if !p.ai { return Err("原文和 AI 译文请分开标记"); }
        let r = p.source.clone().ok_or("这段选区对不上译文位置，不能标记")?;
        range = Some(match range { Some(a) => a.start.min(r.start)..a.end.max(r.end), None => r });
    }
    range.filter(|r| r.start < r.end && r.end <= text_len).ok_or("这段选区对不上译文位置，不能标记")
}

impl Reader {
    fn ai_key(&self) -> Option<String> {
        self.model.book().map(|b| format!("{}|{}", b.doc.item_id, b.doc.revision_id))
    }

    /// The AI translation on screen: its metadata, complete Markdown and blocks.
    pub(super) fn ai_text(&self) -> Option<(&TranslationMeta, &str, &[Block])> {
        if self.tr_markdown().is_empty() { return None; }
        self.chapter_tr.text.as_ref().map(|(m, t, b)| (m, t.as_str(), b.as_slice()))
    }

    /// Per-item reset (another item or revision is fetched).
    pub(super) fn ai_reset(&mut self) {
        let a = &mut self.ai_marks;
        a.ticket += 1;
        a.loaded_for = None;
        a.ledger = None;
        a.busy = false;
        a.notice = None;
        a.deleting.clear();
    }

    /// Reads every AI mark of the book on screen (reads only).
    pub(super) fn ai_load(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.ai_key() else { return };
        if !self.ai_marks.supported {
            self.ai_marks.ledger = None;
            self.ai_marks.loaded_for = None;
            return;
        }
        if self.ai_marks.loaded_for.as_deref() != Some(&key) { self.ai_marks.ledger = None; }
        self.ai_marks.loaded_for = Some(key);
        let Some(item) = self.model.book().map(|b| b.doc.item_id.clone()) else { return };
        self.ai_marks.ticket += 1;
        let ticket = self.ai_marks.ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.ai_annotations_list(&item) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.ai_marks.ticket != ticket { return; }
                match result {
                    Ok(l) => {
                        // An edit of unknown outcome is reconciled by this read; if it
                        // does not show, it stays for 重试同一请求.
                        if let Some(p) = this.ai_marks.pending.clone() {
                            let deleting = matches!(p.change, AiAnnotationChange::Delete { .. });
                            if this.model.book().is_some_and(|b| b.doc.item_id == p.item_id) {
                                if l.annotations.iter().any(|a| a.id == p.id) != deleting {
                                    this.ai_marks.pending = None;
                                    this.ai_marks.notice = Some(tx(if deleting { "旧标记已删除" } else { "标记已保存" }).into());
                                } else {
                                    this.ai_marks.notice = Some(tx("上一个标记的保存结果未知：可以重试同一请求，不会自动重试").into());
                                }
                            }
                        }
                        this.ai_marks.ledger = Some(l);
                    }
                    Err(memex_client::ClientError::Unsupported) => this.ai_marks.supported = false,
                    Err(e) => this.ai_marks.notice = Some(txf("译文标记读取失败：{0}", &[&e.message()])),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// Spans of the current AI marks on the blocks of the AI text on screen, in
    /// server mutation order.
    pub(super) fn ai_spans(&self) -> HashMap<usize, Vec<Span>> {
        let (Some((meta, text, blocks)), Some(ledger)) = (self.ai_text(), self.ai_marks.ledger.as_ref()) else { return HashMap::new() };
        if !self.lr_shows().1 { return HashMap::new(); }
        let Some(revision) = self.model.book().map(|b| b.doc.revision_id.clone()) else { return HashMap::new() };
        let mut listed: Vec<&ListedAiAnnotation> = ledger.annotations.iter().collect();
        listed.sort_by_key(|a| a.order);
        let marks: Vec<(Range<usize>, MarkStyle)> = listed.into_iter()
            .filter_map(|a| ai_ranges(meta, text.len(), &revision, a).map(|r| r.into_iter().map(move |r| (r, a.style))))
            .flatten().collect();
        longread::mark_spans(text, blocks, &marks)
    }

    /// Stale AI marks with a segment in the chapter on screen.
    fn ai_stale(&self) -> Vec<String> {
        let (Some((meta, _, _)), Some(ledger)) = (self.ai_text(), self.ai_marks.ledger.as_ref()) else { return vec![] };
        ledger.annotations.iter().filter(|a| !a.is_current() && a.touches(&meta.original_chapter)).map(|a| a.id.clone()).collect()
    }

    /// One of the five marks on `range` of the AI text on screen.
    pub(super) fn ai_apply(&mut self, range: Range<usize>, style: MarkStyle, cx: &mut Context<Self>) {
        let Some(ledger) = self.ai_marks.ledger.clone() else {
            self.lr_toast(tx("标记列表还没读到，稍后再标").into(), cx);
            return;
        };
        if self.ai_marks.busy || self.ai_marks.pending.is_some() {
            self.lr_toast(tx("上一个标记还没有确认保存").into(), cx);
            return;
        }
        let (Some((meta, text, _)), Some(book)) = (self.ai_text(), self.model.book()) else { return };
        let Some(segment) = ai_segment(meta, text, range) else {
            self.lr_toast(tx("这段选区对不上译文位置，不能标记").into(), cx);
            return;
        };
        let id = format!("mac-aimark-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        let anchor = AiAnchor { revision_id: book.doc.revision_id.clone(), side: AI_SIDE.into(), segments: vec![segment] };
        let change = AiAnnotationChange::Put { annotation: AiAnnotation { id: id.clone(), style, anchor } };
        let pending = PendingAiMark { item_id: book.doc.item_id.clone(), expected: ledger.version, change, id };
        self.ai_send(pending, cx);
        self.lr_toast(txf("已标记为{0}，算作手工重点", &[&tx(style.label())]), cx);
    }

    /// 删除旧标记: the stale marks of this chapter, one edit at a time.
    pub fn ai_delete_stale(&mut self, cx: &mut Context<Self>) {
        if self.ai_marks.busy || self.ai_marks.pending.is_some() { return; }
        self.ai_marks.deleting = self.ai_stale();
        self.ai_delete_next(cx);
    }

    fn ai_delete_next(&mut self, cx: &mut Context<Self>) {
        let (Some(ledger), Some(item)) = (self.ai_marks.ledger.clone(), self.model.book().map(|b| b.doc.item_id.clone())) else { return };
        if self.ai_marks.deleting.is_empty() { return; }
        let id = self.ai_marks.deleting.remove(0);
        self.ai_send(PendingAiMark { item_id: item, expected: ledger.version, change: AiAnnotationChange::Delete { id: id.clone() }, id }, cx);
    }

    /// Sends one AI-mark edit. Any outcome but a receipt is reconciled by
    /// reading; an unknown one keeps the request for 重试同一请求, never resent by itself.
    pub fn ai_send(&mut self, p: PendingAiMark, cx: &mut Context<Self>) {
        self.ai_marks.busy = true;
        self.ai_marks.pending = None;
        let client = self.client.clone();
        let ticket = self.ai_marks.ticket;
        let sent = p.clone();
        let task = cx.background_executor().spawn(async move {
            let outcome = client.ai_annotations_edit(&sent.item_id, &sent.expected, &sent.change);
            let reread = match &outcome { EditOutcome::Saved(_) => None, _ => Some(client.ai_annotations_list(&sent.item_id)) };
            (outcome, reread)
        });
        cx.spawn(async move |this, cx| {
            let (outcome, reread) = task.await;
            this.update(cx, |this, cx| {
                if this.ai_marks.ticket != ticket { return; }
                this.ai_marks.busy = false;
                let deleting = matches!(p.change, AiAnnotationChange::Delete { .. });
                match outcome {
                    EditOutcome::Saved(r) => {
                        if let Some(l) = this.ai_marks.ledger.as_mut() {
                            l.version = r.version;
                            match &p.change {
                                AiAnnotationChange::Put { annotation: a } => {
                                    let order = l.annotations.iter().map(|x| x.order).max().unwrap_or(0) + 1;
                                    let projections = a.anchor.segments.iter().map(|s| ProjectionStatus { chapter: s.chapter.clone(), translation_version: s.translation_version.clone(), sha256: s.sha256.clone(), status: "current".into() }).collect();
                                    l.annotations.retain(|x| x.id != a.id);
                                    l.annotations.push(ListedAiAnnotation { id: a.id.clone(), style: a.style, anchor: a.anchor.clone(), status: "current".into(), order, projections });
                                }
                                AiAnnotationChange::Delete { id } => l.annotations.retain(|x| x.id != *id),
                            }
                        }
                        this.ai_marks.notice = deleting.then(|| tx("旧标记已删除").to_string());
                        if !this.ai_marks.deleting.is_empty() { this.ai_delete_next(cx); }
                    }
                    other => {
                        this.ai_marks.deleting.clear();
                        let listed = reread.and_then(Result::ok);
                        // Reconciled: a put that shows, or a delete that no longer shows, happened.
                        let done = listed.as_ref().is_some_and(|l| l.annotations.iter().any(|a| a.id == p.id) != deleting);
                        if let Some(l) = listed { this.ai_marks.ledger = Some(l); }
                        this.ai_marks.notice = Some(match other {
                            _ if done && deleting => tx("旧标记已删除").into(),
                            _ if done => tx("标记已保存").into(),
                            EditOutcome::Stale => tx("标记没有保存：这本书的译文标记已在别处更改，已重新读取，请再标一次").into(),
                            EditOutcome::Rejected(code) => txf("标记没有保存：{0}", &[&memex_client::ClientError::Daemon(code).message()]),
                            EditOutcome::Unknown(m) => {
                                this.ai_marks.pending = Some(p.clone());
                                txf("结果未知：标记可能没有保存（{0}），可以重试同一请求，不会自动重试", &[&m])
                            }
                            EditOutcome::Saved(_) => unreachable!(),
                        });
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }

    /// The line under the AI caption while the chapter has stale marks.
    pub(super) fn ai_stale_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let stale = self.ai_stale().len();
        if stale == 0 { return None; }
        let theme = cx.theme().clone();
        let busy = self.ai_marks.busy || self.ai_marks.pending.is_some();
        Some(h_flex().flex_none().flex_wrap().gap_2().items_center().px_2().py_1().rounded(px(6.)).bg(theme.secondary).text_xs()
            .child(Icon::new(IconName::Info).xsmall())
            .child(txf("译文已更新，标记未迁移（{0} 条，不再显示，只能删除）", &[&stale]))
            .child(Button::new("ai-marks-delete-stale").xsmall().outline().label(tx("删除旧标记")).disabled(busy)
                .on_click(cx.listener(|r, _, _, cx| r.ai_delete_stale(cx))))
            .into_any_element())
    }

    /// Walks: selects bytes `range` of the AI text on screen, as the renderer's
    /// source map reports a drag selection there.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_select_ai(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.ai_text().and_then(|t| t.1.get(range.clone()).map(str::to_string)).unwrap_or_default();
        self.selected.borrow_mut().clear();
        self.selected.borrow_mut().insert("dev-ai".into(), SelectedPart { source: Some(range), translation: true, ai: true, text, order: 0, at: Some(point(px(700.), px(400.))) });
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Walks: what 重试同一请求 does.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_retry_ai_mark(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.ai_marks.pending.clone() { self.ai_send(p, cx); }
    }

    /// Walks: the AI text on screen (for choosing what to select).
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_ai_text(&self) -> String {
        self.ai_text().map(|t| t.1.to_string()).unwrap_or_default()
    }

    /// Walks: a one-line summary of the AI marks.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_ai_marks(&self) -> String {
        let l = self.ai_marks.ledger.as_ref();
        format!("supported={} loaded={:?} marks={} current={} stale_here={} spans={} pending={} busy={} notice={:?} target={:?}",
            self.ai_marks.supported, self.ai_marks.loaded_for, l.map_or(0, |l| l.annotations.len()), l.map_or(0, |l| l.annotations.iter().filter(|a| a.is_current()).count()),
            self.ai_stale().len(), self.ai_spans().values().map(Vec::len).sum::<usize>(), self.ai_marks.pending.is_some(), self.ai_marks.busy, self.ai_marks.notice,
            self.selection_target().err())
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that glob brings GPUI's own `test` attribute along.
    use super::{ai_target, longread, SelectedPart};
    use crate::model::blocks;
    use memex_client::ai_marks::{ai_ranges, AiAnchor, AiSegment, ListedAiAnnotation, AI_SIDE};
    use memex_client::book::TranslationMeta;
    use memex_client::reader_state::MarkStyle;
    use std::ops::Range;

    fn part(source: Option<Range<usize>>, ai: bool) -> SelectedPart {
        SelectedPart { source, translation: true, ai, text: "x".into(), order: 0, at: None }
    }

    /// Bytes counted by hand: 「## 灯\n\n」 is 2 + 1 + 3 + 2 = 8; the paragraph's raw
    /// 「**甲**：」 is 2 + 3 + 2 + 3 = 10 and 「灯亮了。」 3·3 + 3 = 12. Shown, the
    /// bold markers are gone: 「甲：」 is 6, so the second 「灯亮了」 is shown bytes
    /// 6 + 12 = 18 .. 27, and source bytes 8 + 10 + 12 = 30 .. 39.
    const AI: &str = "## 灯\n\n**甲**：灯亮了。灯亮了。\n\n";

    #[test]
    fn a_selection_maps_through_the_source_map_to_the_repeated_words_own_bytes() {
        let b = &blocks(AI)[1];
        assert_eq!(&b.text[18..27], "灯亮了");
        let raw = &AI[b.start..b.end];
        let second = crate::selection::source_bytes(raw, b.start, b.skip, false, 18..27).unwrap();
        assert_eq!(second, 30..39);
        let first = crate::selection::source_bytes(raw, b.start, b.skip, false, 6..15).unwrap();
        assert_eq!(first, 18..27);
        assert_eq!(&AI[30..39], &AI[18..27], "same words, different bytes");
        // And back: the mark on 30..39 is drawn on exactly the second occurrence.
        let spans = longread::mark_spans(AI, &blocks(AI), &[(30..39, MarkStyle::Yellow)]);
        assert_eq!(spans[&1].iter().map(|s| s.range.clone()).collect::<Vec<_>>(), vec![18..27]);
    }

    /// Bytes by hand: raw 「**托尔斯滕：** 大家不再干等。」 is ** 2, the name 12, ： 3,
    /// ** 2, a space 1, so 「大家」 is source bytes 20..26. Shown 「托尔斯滕： 大家…」:
    /// 「大家」 at 16..22. Displayed as 「Thorsten： 大家…」 (8 bytes for 12): 12..18.
    #[test]
    fn a_relabelled_speaker_keeps_the_same_source_bytes_and_maps_to_none_itself() {
        use crate::selection::{display_to_shown, shown_to_display, source_bytes};
        const RAW: &str = "**托尔斯滕：** 大家不再干等。";
        let b = &blocks(RAW)[0];
        let at = crate::nav::speaker_range(&b.text).unwrap();
        assert_eq!(at, 0..12);
        let src = |shown: Option<std::ops::Range<usize>>| shown.and_then(|r| source_bytes(RAW, b.start, b.skip, false, r));
        let before = src(Some(16..22));
        assert_eq!(before, Some(20..26));
        let label = 0.."Thorsten".len();
        assert_eq!((shown_to_display(16, &at, 8), shown_to_display(22, &at, 8)), (12, 18));
        assert_eq!(src(display_to_shown(12..18, &label, at.len())), before, "same selection, same start/end");
        // Into the replaced name: it starts after the name, at the 「：」 (raw bytes 2 + 12 = 14), never inside it.
        assert_eq!(display_to_shown(3..18, &label, at.len()), Some(12..22));
        assert_eq!(src(display_to_shown(3..18, &label, at.len())), Some(14..26));
        // Only the name: no source bytes, so marking it is refused.
        assert_eq!(display_to_shown(1..6, &label, at.len()), None);
    }

    #[test]
    fn ai_selections_need_the_capability_one_side_and_source_bytes() {
        let len = AI.len();
        assert_eq!(ai_target([&part(Some(30..39), true)], true, len), Ok(30..39));
        assert_eq!(ai_target([&part(Some(30..33), true), &part(Some(18..27), true)], true, len), Ok(18..33));
        assert_eq!(ai_target([&part(Some(30..39), true)], false, len), Err("译文暂不支持保存标记"));
        assert_eq!(ai_target([&part(Some(30..39), true), &part(Some(0..3), false)], true, len), Err("原文和 AI 译文请分开标记"));
        assert_eq!(ai_target([&part(None, true)], true, len), Err("这段选区对不上译文位置，不能标记"));
        assert_eq!(ai_target([&part(Some(40..50), true)], true, len), Err("这段选区对不上译文位置，不能标记"));
    }

    #[test]
    fn a_stale_mark_is_drawn_on_no_segment_even_where_its_own_segment_is_current() {
        let meta = |chapter: &str, version: &str| TranslationMeta {
            version: version.into(), item_id: "i".into(), revision_id: "r".into(), original_chapter: chapter.into(), source_sha256: "s".into(),
            target_language: "zh-CN".into(), sha256: format!("h-{chapter}"), bytes: AI.len(), provenance: "machine_translation".into(),
            route: serde_json::from_value(serde_json::json!({"provider_id": "p", "model_id": "m"})).unwrap(),
        };
        let seg = |chapter: &str, version: &str, r: Range<usize>| AiSegment { chapter: chapter.into(), translation_version: version.into(), sha256: format!("h-{chapter}"), start: r.start, end: r.end };
        let mark = |status: &str, segments| ListedAiAnnotation { id: "m".into(), style: MarkStyle::Red, anchor: AiAnchor { revision_id: "r".into(), side: AI_SIDE.into(), segments }, status: status.into(), order: 1, projections: vec![] };
        let (c1, c2) = (meta("c1", "v1"), meta("c2", "v2"));
        let two = |status| mark(status, vec![seg("c1", "v1", 30..39), seg("c2", "v2", 0..3)]);
        assert_eq!(ai_ranges(&c1, AI.len(), "r", &two("current")), Some(vec![30..39]));
        assert_eq!(ai_ranges(&c2, AI.len(), "r", &two("current")), Some(vec![0..3]));
        // c2 was retranslated: the server marks the whole mark stale.
        assert_eq!(ai_ranges(&c1, AI.len(), "r", &two("stale")), None);
        // A segment whose version is not the text on screen never binds, even if listed current.
        assert_eq!(ai_ranges(&meta("c1", "v9"), AI.len(), "r", &two("current")), None);
        // Another revision does not bind either.
        assert_eq!(ai_ranges(&c1, AI.len(), "r2", &two("current")), None);
    }
}
