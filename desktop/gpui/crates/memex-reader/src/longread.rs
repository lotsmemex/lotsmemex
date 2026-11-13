//! 长文阅读 v2 rev10 on the Mac: ≡ 目录 (floating, 钉住, 常驻), the 重点
//! switch (四档), the continuous rail with its bump and arched thumb, the 重点
//! dial pulled out to the left, the three knobs with 上次读到 and 书签, the
//! selection 浮动条 with five manual marks, the speaker bubble, B2 position
//! and bookmarks over the owner socket, and `reader_annotations_v1` marks.

use super::*;
use crate::model::Row as LRow;
use crate::nav::{self, Filter, Stop};
use memex_client::book::Edition;
use memex_client::chat::{ChatRole, StoredTurn};
use memex_client::reader_state::{
    anchor_at, body_at, ranges_of, segments_of, Annotation, AnnotationChange, AnnotationLedger, Bookmark, EditOutcome, ListedAnnotation, MarkAnchor, MarkStyle,
    ReaderState, VersionedState,
};
use std::cell::{Cell, RefCell};

actions!(memex_reader, [MarkBold, MarkRed, MarkBlue, MarkUnderline, MarkYellow]);

/// Key context of the reading area (its root takes focus on a click).
pub const READER_CONTEXT: &str = "MemexReader";

/// ⌘B 加粗, ⌘I 标红, ` 标蓝, ⌘U 下划线, ⌘Y 高亮 — in the reading area only.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-b", MarkBold, Some(READER_CONTEXT)),
        KeyBinding::new("cmd-i", MarkRed, Some(READER_CONTEXT)),
        KeyBinding::new("`", MarkBlue, Some(READER_CONTEXT)),
        KeyBinding::new("cmd-u", MarkUnderline, Some(READER_CONTEXT)),
        KeyBinding::new("cmd-y", MarkYellow, Some(READER_CONTEXT)),
    ]
}

const TH: f32 = 26.;
const BX: f32 = 28.;
const BUMP: f32 = 8.;
const TR: f32 = 18.;
/// Room right of the long-read text for the knobs and the rail (the rail is the
/// rightmost 38 px, its track `38 - BX` from the edge).
pub(super) const GUTTER: f32 = 76.;
/// Widest long-read text line (wider than the 720 px elsewhere: less blank beside the rail).
pub(super) const MEASURE: f32 = 860.;
/// The knobs' centre line, from the same edge: halfway between the text and the track.
const KNOB_AXIS: f32 = (GUTTER + 38. - BX) / 2.;
/// Characters of context before a keyword on a Mac dial card.
const CONTEXT_CHARS: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum Drag {
    Thumb { x0: f32, y0: f32, moved: bool },
    Bookmark { id: String, y: f32 },
    Toc { y0: f32, top0: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dial {
    pub c: f32,
    pub c0: f32,
    pub y0: f32,
    pub cy: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Press {
    pub y0: f32,
    pub long: bool,
    pub was_open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnobKey {
    Stop(Stop),
    Last,
    Bookmark(String),
}

/// One card of the 重点 dial / one tick of the rail.
#[derive(Debug, Clone)]
pub struct Term {
    /// List index of the row.
    pub ix: usize,
    pub manual: bool,
    pub ctx: String,
    pub kw: String,
    pub pct: u8,
    pub color: Hsla,
}

/// The mark edit sent last whose reply was lost: kept as it was (same version,
/// same change) until a read shows it or the user sends exactly it again.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingMark {
    pub item_id: String,
    pub expected: String,
    pub change: AnnotationChange,
    pub id: String,
}

pub struct LongRead {
    pub filter: Filter,
    pub menu_open: bool,
    pub toc_open: bool,
    pub toc_pinned: bool,
    pub toc_docked: bool,
    pub toc_top: f32,
    pub drag: Option<Drag>,
    pub dial: Option<Dial>,
    pub list_open: bool,
    pub hot: Option<KnobKey>,
    pub press: Option<Press>,
    pub knobs_awake: bool,
    pub last_read: Option<usize>,
    pub toast: Option<String>,
    toast_ticket: u64,
    pub area: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub rail: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub knob_rows: Rc<RefCell<Vec<(KnobKey, Bounds<Pixels>)>>>,
    pub menu_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub terms: Rc<Vec<Term>>,
    // B2 position and bookmarks.
    pub source: Option<Edition>,
    pub state: Option<VersionedState>,
    pub state_error: Option<String>,
    pub visit_id: String,
    pub saved_top: Option<usize>,
    attempted_top: Option<usize>,
    save_ticket: u64,
    pub notice: Option<String>,
    /// The body byte to bring back once the rows are laid out.
    restore: Option<usize>,
    // Five-style marks.
    pub marks_supported: bool,
    pub ledger: Option<AnnotationLedger>,
    /// Marks kept only in this app session (no `reader_annotations_v1`), by item|revision.
    pub session: HashMap<String, Vec<(Range<usize>, MarkStyle)>>,
    pub pending: Option<PendingMark>,
    pub mark_busy: bool,
    pub mark_notice: Option<String>,
    pub chat: Rc<Vec<StoredTurn>>,
    pub loaded_for: Option<(String, String)>,
    /// Translation-side speakers of the text on screen, keyed by revision and texts.
    speakers_t: RefCell<Option<(String, Rc<nav::TranslatedSpeakers>)>>,
}

impl Default for LongRead {
    fn default() -> Self {
        LongRead {
            filter: Filter::All, menu_open: false, toc_open: false, toc_pinned: false, toc_docked: false, toc_top: 8., drag: None, dial: None,
            list_open: false, hot: None, press: None, knobs_awake: false, last_read: None, toast: None, toast_ticket: 0,
            area: Default::default(), rail: Default::default(), knob_rows: Default::default(), menu_anchor: Default::default(), terms: Default::default(),
            source: None, state: None, state_error: None, visit_id: String::new(), saved_top: None, attempted_top: None, save_ticket: 0, notice: None, restore: None,
            marks_supported: false, ledger: None, session: HashMap::new(), pending: None, mark_busy: false, mark_notice: None, chat: Default::default(), loaded_for: None,
            speakers_t: Default::default(),
        }
    }
}

fn fresh_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{prefix}-{nanos:x}")
}

fn gold() -> Hsla { hsla(30. / 360., 0.78, 0.50, 1.) }
fn gold_wash(dark: bool) -> Hsla { if dark { hsla(32. / 360., 0.40, 0.20, 1.) } else { hsla(36. / 360., 0.90, 0.92, 1.) } }
fn manual_kind(style: MarkStyle) -> &'static str {
    match style {
        MarkStyle::Bold => "manual_bold",
        MarkStyle::Red => "manual_red",
        MarkStyle::Blue => "manual_blue",
        MarkStyle::Underline => "manual_underline",
        MarkStyle::Yellow => "manual_yellow",
    }
}
/// Where a selection's mark goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MarkTarget {
    /// Body bytes of the item (`reader_annotations_v1`).
    Source(Range<usize>),
    /// Bytes of the complete AI translation of the chapter on screen.
    Ai(Range<usize>),
}

/// Spans of `marks` (byte ranges of `markdown`) on its `blocks`: the renderer's
/// source map read backwards, so a mark covers exactly its own bytes.
pub(super) fn mark_spans(markdown: &str, blocks: &[Block], marks: &[(Range<usize>, MarkStyle)]) -> HashMap<usize, Vec<Span>> {
    let mut spans: HashMap<usize, Vec<Span>> = HashMap::new();
    if marks.is_empty() { return spans; }
    for (index, block) in blocks.iter().enumerate() {
        if block.kind == BlockKind::Image { continue; }
        let Some(raw) = markdown.get(block.start..block.end) else { continue };
        let inline = crate::inline::Inline::parse(raw, block.skip);
        for (range, style) in marks {
            let (start, end) = (range.start.max(block.start), range.end.min(block.end));
            if start >= end { continue; }
            let shown = if block.kind == BlockKind::Code { Some(start - block.start..end - block.start) } else { inline.shown(start - block.start..end - block.start) };
            if let Some(r) = shown.filter(|r| r.start < r.end) {
                spans.entry(index).or_default().push(Span { range: r, style: memex_client::insights::HighlightStyle::Other(manual_kind(*style).into()), label: Some(tx("手工").into()) });
            }
        }
    }
    spans
}

/// 「谁在说」: the speaker's initial and name, and 对照不可用 when the chosen
/// reference name could not be found.
pub(super) fn speaker_bubble(name: String, unavailable: bool, p: &Palette, theme: &gpui_kit::component::Theme) -> AnyElement {
    h_flex().absolute().top(px(8.)).left(px(8.)).gap_1p5().items_center().pl(px(3.)).pr_2p5().py(px(3.)).rounded_full().bg(theme.background).border_1().border_color(theme.border).shadow_sm().text_xs()
        .child(div().size(px(20.)).rounded_full().bg(p.rose).text_color(theme.background).flex().items_center().justify_center().child(nav::initial(&name)))
        .child(div().text_color(theme.foreground).child(name)).child(div().text_color(theme.muted_foreground).child(tx("正在说")))
        .when(unavailable, |d| d.child(div().text_color(theme.muted_foreground).opacity(0.75).child(tx("· 对照不可用"))))
        .into_any_element()
}

fn is_manual(style: &memex_client::insights::HighlightStyle) -> bool {
    matches!(style, memex_client::insights::HighlightStyle::Other(k) if k.starts_with("manual_"))
}

impl Reader {
    /// Per-item reset when another item (or revision) is fetched.
    pub(super) fn lr_reset(&mut self) {
        let lr = &mut self.lr;
        lr.drag = None;
        lr.dial = None;
        lr.list_open = false;
        lr.hot = None;
        lr.press = None;
        lr.menu_open = false;
        lr.last_read = None;
        lr.source = None;
        lr.state = None;
        lr.state_error = None;
        lr.saved_top = None;
        lr.attempted_top = None;
        lr.save_ticket += 1;
        lr.notice = None;
        lr.restore = None;
        lr.ledger = None;
        lr.mark_notice = None;
        lr.mark_busy = false;
        lr.chat = Default::default();
        lr.loaded_for = None;
        if !lr.toc_docked { lr.toc_open = lr.toc_open && lr.toc_pinned; }
    }

    /// Reads, off the UI thread, what the long-read view needs of an ordinary
    /// item: its source toc (for real `section-*` anchors), B2 state, the marks
    /// and the stored AI conversation. Reads only; nothing is generated.
    pub(super) fn lr_load(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.model.detail() else { return };
        if self.model.book().is_some() { return; }
        let (item, revision) = (detail.canonical.item_id.clone(), detail.canonical.revision_id.clone());
        self.lr.loaded_for = Some((item.clone(), revision.clone()));
        self.lr.visit_id = fresh_id("mac-visit");
        self.pass_load(item.clone(), revision.clone(), cx);
        let client = self.client.clone();
        let ticket = self.ticket;
        let task = cx.background_executor().spawn(async move {
            let source = client.reader_toc(&item, &revision).ok().and_then(|d| d.editions.into_iter().find(|e| e.side == "source"));
            let state = client.reader_state(&item);
            let supported = client.annotations_supported();
            let ledger = supported.then(|| client.annotations_list(&item));
            let chat = client.conversation_read(&item).unwrap_or_default();
            (source, state, supported, ledger, chat)
        });
        cx.spawn(async move |this, cx| {
            let (source, state, supported, ledger, chat) = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket { return; }
                let lr = &mut this.lr;
                lr.marks_supported = supported;
                lr.chat = Rc::new(chat);
                match ledger {
                    Some(Ok(l)) => lr.ledger = Some(l),
                    Some(Err(e)) => lr.mark_notice = Some(txf("标记读取失败：{0}", &[&e.message()])),
                    None => {}
                }
                lr.source = source;
                match state {
                    Ok(s) => {
                        let rev = this.model.detail().map(|d| d.canonical.revision_id.clone()).unwrap_or_default();
                        if let Some(pos) = &s.state.position {
                            match this.lr.source.as_ref().and_then(|src| body_at(src, &rev, pos)) {
                                Some(body) => this.lr.restore = Some(body),
                                // Bound to another revision: never guessed onto the new text.
                                None => this.lr.notice = Some(tx("文章已更新，从开头开始").into()),
                            }
                        }
                        this.lr.state = Some(s);
                    }
                    Err(memex_client::ClientError::Unsupported) => {}
                    Err(e) => this.lr.state_error = Some(e.message()),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    // ----- rows and positions -------------------------------------------

    fn lr_rows(&self) -> Vec<LRow> {
        self.model.layout().rows
    }

    /// The original block shown by layout row `row`.
    fn row_block(rows: &[LRow], row: usize) -> Option<usize> {
        match rows.get(row)? {
            LRow::Original(i) | LRow::Shared(i) | LRow::Pair(i) => Some(*i),
            _ => None,
        }
    }

    /// The layout row showing the original block that holds body byte `body`.
    fn row_of_body(&self, rows: &[LRow], body: usize) -> Option<usize> {
        let blocks = self.model.original();
        let block = blocks.iter().rposition(|b| b.start <= body)?;
        (0..rows.len()).filter(|r| Self::row_block(rows, *r).is_some_and(|b| b <= block)).last()
    }

    fn top_ix(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }

    /// Where reading is now, for the knobs (list index; at the very end, past it).
    fn knob_position(&self, rows: usize) -> f32 {
        if rows > 0 && self.reading_pct >= 100 { return (LEAD + rows) as f32; }
        self.top_ix() as f32
    }

    fn knob_stops(rows: usize) -> [f32; 4] {
        let last = (LEAD + rows.max(1) - 1) as f32;
        [0., LEAD as f32, last, last + 1.]
    }

    /// Scrolls to list index `ix`; a real move leaves 上次读到 behind.
    pub fn lr_jump(&mut self, ix: usize, remember: bool, cx: &mut Context<Self>) {
        let cur = self.top_ix();
        if remember && cur.abs_diff(ix) >= 1 { self.lr.last_read = Some(cur); }
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
        cx.notify();
    }

    pub fn lr_go_stop(&mut self, stop: Stop, remember: bool, cx: &mut Context<Self>) {
        let rows = self.lr_rows().len();
        if remember { self.lr.last_read = Some(self.top_ix()); }
        match stop {
            Stop::Minutes => self.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) }),
            Stop::Start => self.list.scroll_to(ListOffset { item_ix: LEAD, offset_in_item: px(0.) }),
            Stop::End => self.list.scroll_to(ListOffset { item_ix: LEAD + rows.max(1) - 1, offset_in_item: px(0.) }),
            Stop::Chat => self.list.scroll_to_end(),
        }
        self.lr_toast(tx(stop.label()).to_string(), cx);
        cx.notify();
    }

    /// ▲ / ▼.
    pub fn lr_step(&mut self, up: bool, cx: &mut Context<Self>) {
        let rows = self.lr_rows().len();
        let (stop, remember) = nav::step(self.knob_position(rows), Self::knob_stops(rows), up);
        self.lr.knobs_awake = true;
        self.lr_go_stop(stop, remember, cx);
    }

    /// 上次读到: goes back, and the place left becomes 上次读到 in its turn.
    pub fn lr_go_last(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.lr.last_read else { return };
        let cur = self.top_ix();
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
        self.lr.last_read = Some(cur);
        cx.notify();
    }

    pub fn lr_toast(&mut self, text: String, cx: &mut Context<Self>) {
        self.lr.toast = Some(text);
        self.lr.toast_ticket += 1;
        let ticket = self.lr.toast_ticket;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1600)).await;
            this.update(cx, |this, cx| if this.lr.toast_ticket == ticket { this.lr.toast = None; cx.notify(); }).ok();
        }).detach();
        cx.notify();
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.lr.filter = filter;
        self.insights.emphasis = filter.shows_ai();
        if self.lr.dial.is_some() && self.lr.terms.is_empty() { self.lr.dial = None; }
        self.list.remeasure();
        cx.notify();
    }

    // ----- B2 saves -------------------------------------------------------

    /// Called each frame: a settled new top row is saved after a pause.
    pub(super) fn lr_track_position(&mut self, rows: &[LRow], cx: &mut Context<Self>) {
        if let Some(body) = self.lr.restore {
            if !rows.is_empty() {
                self.lr.restore = None;
                if let Some(row) = self.row_of_body(rows, body) {
                    self.list.scroll_to(ListOffset { item_ix: LEAD + row, offset_in_item: px(0.) });
                    self.lr.saved_top = Some(LEAD + row);
                    self.lr.attempted_top = Some(LEAD + row);
                }
            }
            return;
        }
        if self.lr.state.is_none() || self.lr.source.is_none() || self.lr.drag.is_some() { return; }
        let top = self.top_ix();
        if self.lr.attempted_top == Some(top) || self.lr.saved_top == Some(top) { return; }
        // Only a row of the original side has a source offset (译文 rows do not).
        if top < LEAD || Self::row_block(rows, top - LEAD).is_none() { return; }
        self.lr.attempted_top = Some(top);
        self.lr.save_ticket += 1;
        let ticket = self.lr.save_ticket;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            this.update(cx, |this, cx| if this.lr.save_ticket == ticket { this.lr_save(Some(top), None, cx) }).ok();
        }).detach();
    }

    /// Saves the position at list index `top` (or keeps the saved one) and,
    /// when given, the new bookmarks. A conflict reads and reconciles once.
    fn lr_save(&mut self, top: Option<usize>, bookmarks: Option<Vec<Bookmark>>, cx: &mut Context<Self>) {
        let (Some(state), Some(src), Some(detail)) = (self.lr.state.clone(), self.lr.source.clone(), self.model.detail()) else { return };
        let item = detail.canonical.item_id.clone();
        let rev = detail.canonical.revision_id.clone();
        let rows = self.lr_rows();
        let position = match top {
            Some(top) => top.checked_sub(LEAD).and_then(|r| Self::row_block(&rows, r)).and_then(|b| self.model.original().get(b)).and_then(|b| anchor_at(&src, &rev, b.start)),
            None => state.state.position.clone().filter(|p| body_at(&src, &rev, p).is_some()),
        };
        let mut next = ReaderState { visit_id: self.lr.visit_id.clone(), position, bookmarks: bookmarks.clone().unwrap_or_else(|| state.state.bookmarks.clone()) };
        let client = self.client.clone();
        let ticket = self.ticket;
        let task = cx.background_executor().spawn(async move {
            match client.reader_save_state(&item, &state.version, &next) {
                Err(memex_client::ClientError::Daemon(code)) if code == "stale_reader_state" => {
                    // Another device saved: keep its bookmarks (plus ours when we
                    // were adding), never overwrite blindly; then save once more.
                    let fresh = client.reader_state(&item)?;
                    let mut merged = fresh.state.bookmarks.clone();
                    if let Some(ours) = &bookmarks {
                        for b in ours { if !merged.iter().any(|m| m.id == b.id) { merged.push(b.clone()); } }
                        merged.retain(|m| ours.iter().any(|b| b.id == m.id) || !state.state.bookmarks.iter().any(|b| b.id == m.id));
                    }
                    next.bookmarks = merged;
                    client.reader_save_state(&item, &fresh.version, &next)
                }
                other => other,
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket { return; }
                match result {
                    Ok(saved) => {
                        if top.is_some() { this.lr.saved_top = top; }
                        this.lr.state = Some(saved);
                        this.lr.state_error = None;
                    }
                    // Unknown or rejected: the state read stays as it was; the next
                    // move saves again from it (an identical replay is accepted).
                    Err(e) => this.lr.state_error = Some(e.message()),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// The bookmarks of the current revision with their list index.
    pub(super) fn lr_bookmarks(&self, rows: &[LRow]) -> Vec<(Bookmark, usize)> {
        let (Some(state), Some(src), Some(detail)) = (&self.lr.state, &self.lr.source, self.model.detail()) else { return vec![] };
        let mut out: Vec<(Bookmark, usize)> = state.state.bookmarks.iter().filter_map(|b| {
            let body = body_at(src, &detail.canonical.revision_id, &b.anchor)?;
            Some((b.clone(), LEAD + self.row_of_body(rows, body)?))
        }).collect();
        out.sort_by_key(|(_, ix)| *ix);
        out
    }

    /// Double click on the thumb: a bookmark at the start of the paragraph at the viewport's middle line.
    pub fn lr_add_bookmark(&mut self, cx: &mut Context<Self>) {
        let rows = self.lr_rows();
        let (Some(src), Some(state), Some(detail)) = (self.lr.source.clone(), self.lr.state.clone(), self.model.detail()) else {
            let why = self.lr.state_error.clone().unwrap_or_else(|| tx("这篇不能保存阅读位置").into());
            self.lr_toast(txf("不能加书签：{0}", &[&why]), cx);
            return;
        };
        let rev = detail.canonical.revision_id.clone();
        let view = self.list.viewport_bounds();
        let mid = view.top() + view.size.height / 2.;
        let top = self.top_ix();
        let mut row = None;
        for ix in top..(top + 80).min(LEAD + rows.len()) {
            let Some(b) = self.list.bounds_for_item(ix) else { break };
            if ix >= LEAD && Self::row_block(&rows, ix - LEAD).is_some() { row = Some(ix - LEAD); }
            if b.bottom() > mid && row.is_some() { break; }
        }
        let Some(anchor) = row.and_then(|r| Self::row_block(&rows, r)).and_then(|b| self.model.original().get(b)).and_then(|b| anchor_at(&src, &rev, b.start)) else { return };
        if state.state.bookmarks.iter().any(|b| b.anchor == anchor) {
            self.lr_toast(tx("这一段已有书签").into(), cx);
            return;
        }
        let mut list = state.state.bookmarks.clone();
        list.push(Bookmark { id: fresh_id("mac-bm"), label: tx("书签").into(), anchor });
        self.lr_save(None, Some(list), cx);
        self.lr_toast(tx("已加书签，可按住上下拖动").into(), cx);
    }

    /// A bookmark dropped at window `y`: it moves to the nearest paragraph start.
    pub fn lr_drop_bookmark(&mut self, id: &str, y: f32, cx: &mut Context<Self>) {
        let rows = self.lr_rows();
        let (Some(src), Some(state), Some(detail)) = (self.lr.source.clone(), self.lr.state.clone(), self.model.detail()) else { return };
        let rev = detail.canonical.revision_id.clone();
        let top = self.top_ix();
        let starts: Vec<(usize, f32)> = (top..(top + 80).min(LEAD + rows.len()))
            .filter(|ix| *ix >= LEAD && Self::row_block(&rows, ix - LEAD).is_some())
            .filter_map(|ix| self.list.bounds_for_item(ix).map(|b| (ix, f32::from(b.top()))))
            .collect();
        let Some(ix) = nav::snap(&starts, y) else { return };
        let Some(anchor) = Self::row_block(&rows, ix - LEAD).and_then(|b| self.model.original().get(b)).and_then(|b| anchor_at(&src, &rev, b.start)) else { return };
        let mut list = state.state.bookmarks.clone();
        if let Some(b) = list.iter_mut().find(|b| b.id == id) { b.anchor = anchor; }
        self.lr_save(None, Some(list), cx);
        self.lr_toast(tx("书签已移到这一段开头").into(), cx);
    }

    // ----- marks ----------------------------------------------------------

    fn session_key(&self) -> Option<String> {
        self.model.detail().map(|d| format!("{}|{}", d.canonical.item_id, d.canonical.revision_id))
    }

    /// Body byte ranges of the manual marks shown on this revision, with their styles.
    pub(super) fn manual_ranges(&self) -> Vec<(Range<usize>, MarkStyle)> {
        let mut out = vec![];
        if let (Some(ledger), Some(src), Some(detail)) = (&self.lr.ledger, &self.lr.source, self.model.detail()) {
            for a in ledger.annotations.iter().filter(|a| a.is_current()) {
                for r in ranges_of(src, &detail.canonical.revision_id, &a.anchor).unwrap_or_default() { out.push((r, a.style)); }
            }
        }
        if let Some(key) = self.session_key() { out.extend(self.lr.session.get(&key).cloned().unwrap_or_default()); }
        out
    }

    /// Current manual marks (a mark across chapters counts once; legacy underlines once each).
    pub(super) fn manual_count(&self) -> usize {
        let saved = match (&self.lr.ledger, &self.lr.source, self.model.detail()) {
            (Some(l), Some(src), Some(d)) => l.annotations.iter().filter(|a| a.is_current() && ranges_of(src, &d.canonical.revision_id, &a.anchor).is_some()).count(),
            _ => 0,
        };
        let session = self.session_key().and_then(|k| self.lr.session.get(&k)).map_or(0, Vec::len);
        let legacy = self.annotations.as_ref().map_or(0, |a| a.underlines.len());
        saved + session + legacy
    }

    pub(super) fn stale_marks(&self) -> usize {
        self.lr.ledger.as_ref().map_or(0, |l| l.annotations.iter().filter(|a| !a.is_current()).count())
    }

    /// Spans of the manual marks on the original blocks (as the legacy underline overlay).
    pub(super) fn manual_spans(&self, markdown: &str, blocks: &[Block]) -> HashMap<usize, Vec<Span>> {
        mark_spans(markdown, blocks, &self.manual_ranges())
    }

    /// What the selection can be marked as: its body byte range (or, in an AI
    /// chapter translation, its byte range there), or why not.
    pub(super) fn selection_target(&self) -> Result<MarkTarget, String> {
        let parts = self.selected.borrow();
        if parts.values().all(|p| p.text.trim().is_empty()) { return Err(String::new()); }
        if parts.values().any(|p| p.ai) {
            if self.item_style().script != memex_client::opencc::Script::Original { return Err(tx("简繁转换显示时不能标记").into()); }
            let len = self.ai_text().map_or(0, |t| t.1.len());
            return super::ai_marks::ai_target(parts.values(), self.ai_marks.supported, len).map(MarkTarget::Ai).map_err(|w| tx(w).to_string());
        }
        if parts.values().any(|p| p.translation) { return Err(tx("译文暂不支持保存标记").into()); }
        if self.model.book().is_some() { return Err(tx("书籍章节暂不支持保存标记").into()); }
        if self.item_style().script != memex_client::opencc::Script::Original { return Err(tx("简繁转换显示时不能标记").into()); }
        let ranges: Option<Vec<Range<usize>>> = parts.values().map(|p| p.source.clone()).collect();
        let ranges = ranges.ok_or_else(|| tx("这段选区对不上原文位置，不能标记").to_string())?;
        let start = ranges.iter().map(|r| r.start).min().unwrap_or(0);
        let end = ranges.iter().map(|r| r.end).max().unwrap_or(0);
        if start >= end { return Err(String::new()); }
        Ok(MarkTarget::Source(start..end))
    }

    pub(super) fn has_selection(&self) -> bool {
        self.selected.borrow().values().any(|p| !p.text.trim().is_empty())
    }

    fn selected_text(&self) -> (String, bool) {
        let parts = self.selected.borrow();
        let mut v: Vec<_> = parts.values().collect();
        v.sort_by_key(|p| p.order);
        (v.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n"), v.iter().any(|p| p.translation))
    }

    /// One of the five marks on the selection.
    pub fn apply_mark(&mut self, style: MarkStyle, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.selection_target() {
            Ok(r) => r,
            Err(why) => { if !why.is_empty() { self.lr_toast(why, cx); } return; }
        };
        self.lr.filter = self.lr.filter.after_mark();
        self.insights.emphasis = self.lr.filter.shows_ai();
        gpui_kit::base::TextSelection::clear(window, cx);
        self.selected.borrow_mut().clear();
        let range = match target {
            MarkTarget::Source(r) => r,
            MarkTarget::Ai(r) => return self.ai_apply(r, style, cx),
        };
        let label = tx(style.label());
        if !self.lr.marks_supported {
            if let Some(key) = self.session_key() { self.lr.session.entry(key).or_default().push((range, style)); }
            self.lr.mark_notice = Some(tx("本机暂存，未同步保存").into());
            self.lr_toast(txf("已标记为{0}，算作手工重点", &[&label]), cx);
            self.list.remeasure();
            return;
        }
        let (Some(src), Some(ledger), Some(detail)) = (self.lr.source.clone(), self.lr.ledger.clone(), self.model.detail()) else {
            self.lr_toast(tx("标记列表还没读到，稍后再标").into(), cx);
            return;
        };
        if self.lr.mark_busy || self.lr.pending.is_some() {
            self.lr_toast(tx("上一个标记还没有确认保存").into(), cx);
            return;
        }
        let rev = detail.canonical.revision_id.clone();
        let anchor = MarkAnchor { revision_id: rev, side: src.side.clone(), sha256: src.sha256.clone(), segments: segments_of(&src, range) };
        let id = fresh_id("mac-mark");
        let change = AnnotationChange::Put { annotation: Annotation { id: id.clone(), style, anchor } };
        let pending = PendingMark { item_id: detail.canonical.item_id.clone(), expected: ledger.version, change, id };
        self.lr_send_mark(pending, cx);
        self.lr_toast(txf("已标记为{0}，算作手工重点", &[&label]), cx);
    }

    /// Sends one mark edit; a lost reply is reconciled by reading, never resent by itself.
    pub fn lr_send_mark(&mut self, p: PendingMark, cx: &mut Context<Self>) {
        self.lr.mark_busy = true;
        self.lr.pending = None;
        let client = self.client.clone();
        let ticket = self.ticket;
        let sent = p.clone();
        let task = cx.background_executor().spawn(async move {
            let outcome = client.annotations_edit(&sent.item_id, &sent.expected, &sent.change);
            let reread = match &outcome { EditOutcome::Saved(_) => None, _ => Some(client.annotations_list(&sent.item_id)) };
            (outcome, reread)
        });
        cx.spawn(async move |this, cx| {
            let (outcome, reread) = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket { return; }
                this.lr.mark_busy = false;
                let put = match &p.change { AnnotationChange::Put { annotation } => Some(annotation.clone()), AnnotationChange::Delete { .. } => None };
                match outcome {
                    EditOutcome::Saved(r) => {
                        if let Some(l) = this.lr.ledger.as_mut() {
                            l.version = r.version;
                            if let Some(a) = put { l.annotations.retain(|x| x.id != a.id); l.annotations.push(ListedAnnotation { id: a.id, style: a.style, anchor: a.anchor, status: "current".into() }); }
                        }
                        this.lr.mark_notice = None;
                    }
                    other => {
                        let listed = reread.and_then(Result::ok);
                        let shown = listed.as_ref().is_some_and(|l| l.annotations.iter().any(|a| a.id == p.id));
                        if let Some(l) = listed { this.lr.ledger = Some(l); }
                        this.lr.mark_notice = Some(match other {
                            _ if shown => tx("标记已保存").into(),
                            EditOutcome::Stale => tx("标记没有保存：这篇的标记已在别处更改，已重新读取，请再标一次").into(),
                            EditOutcome::Rejected(code) => txf("标记没有保存：{0}", &[&memex_client::ClientError::Daemon(code).message()]),
                            EditOutcome::Unknown(m) => {
                                this.lr.pending = Some(p.clone());
                                txf("结果未知：标记可能没有保存（{0}），可以重试同一请求，不会自动重试", &[&m])
                            }
                            EditOutcome::Saved(_) => unreachable!(),
                        });
                    }
                }
                this.list.remeasure();
                cx.notify();
            }).ok();
        }).detach();
    }

    /// AI on the selection: the conversation box opens with it quoted; nothing is sent.
    pub fn ask_about_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, translation) = self.selected_text();
        let text = text.trim().to_string();
        if text.is_empty() { return; }
        if text.len() > 6000 {
            self.lr_toast(tx("选中文字太长，请缩短后再问").into(), cx);
            return;
        }
        let rev = self.model.detail().map(|d| d.canonical.revision_id.clone()).unwrap_or_default();
        let side = if translation { tx("AI 译文") } else { tx("原文") };
        let prefill = format!("{}\n「{}」\n", txf("引用（{0}，版本 {1}）：", &[&side, &rev]), text);
        gpui_kit::base::TextSelection::clear(window, cx);
        self.selected.borrow_mut().clear();
        self.lr.mark_notice = Some(tx("已放进对话框，点发送才会提问；发送时仍会附带这篇的正文与元数据上下文").into());
        cx.emit(ReaderEvent::Ask(prefill));
        cx.notify();
    }

    pub(super) fn on_mark_key(&mut self, style: MarkStyle, window: &mut Window, cx: &mut Context<Self>) {
        let input_focused = !self.focus.is_focused(window) || self.editing.visible;
        let (key, cmd) = match style {
            MarkStyle::Bold => ("b", true),
            MarkStyle::Red => ("i", true),
            MarkStyle::Blue => ("`", false),
            MarkStyle::Underline => ("u", true),
            MarkStyle::Yellow => ("y", true),
        };
        match nav::mark_for_key(key, cmd, false, self.has_selection(), input_focused) {
            Some(style) => self.apply_mark(style, window, cx),
            None => cx.propagate(),
        }
    }

    // ----- terms (dial cards and rail ticks) --------------------------------

    pub(super) fn lr_terms(&self, rows: &[LRow], spans_o: &HashMap<usize, Vec<Span>>, spans_t: &HashMap<usize, Vec<Span>>, p: &Palette) -> Vec<Term> {
        let (original, translation) = (self.model.original(), self.model.translation());
        let mut out = vec![];
        let color = |s: &memex_client::insights::HighlightStyle| match s {
            _ if is_manual(s) => gold(),
            memex_client::insights::HighlightStyle::Positive => p.pos,
            memex_client::insights::HighlightStyle::Negative => p.neg,
            memex_client::insights::HighlightStyle::Bold => p.fg.opacity(0.7),
            _ => hsla(44. / 360., 0.85, 0.52, 1.),
        };
        let mut push = |row: usize, block: Option<&Block>, spans: Option<&Vec<Span>>| {
            let (Some(b), Some(spans)) = (block, spans) else { return };
            let mut seen: Vec<Range<usize>> = vec![];
            let mut sorted: Vec<&Span> = spans.iter().collect();
            sorted.sort_by_key(|s| s.range.start);
            for s in sorted {
                // A mark split over styles of the same stretch is one card.
                if seen.iter().any(|r| *r == s.range) { continue; }
                seen.push(s.range.clone());
                let Some(kw) = b.text.get(s.range.clone()) else { continue };
                out.push(Term { ix: LEAD + row, manual: is_manual(&s.style), ctx: nav::context_before(&b.text, s.range.start, CONTEXT_CHARS), kw: nav::keyword(kw, 12), pct: nav::percent(row, rows.len()), color: color(&s.style) });
            }
        };
        for (row, r) in rows.iter().enumerate() {
            match *r {
                LRow::Original(i) | LRow::Shared(i) => push(row, original.get(i), spans_o.get(&i)),
                LRow::Translation(i) => push(row, translation.get(i), spans_t.get(&i)),
                LRow::Pair(i) => { push(row, original.get(i), spans_o.get(&i)); push(row, translation.get(i), spans_t.get(&i)); }
                LRow::Aligned(_) => {}
            }
        }
        out
    }

    fn dial_near(&self) -> f32 {
        let cur = self.top_ix();
        self.lr.terms.iter().enumerate().min_by_key(|(_, t)| t.ix.abs_diff(cur)).map_or(0., |(k, _)| k as f32)
    }

    pub fn lr_open_dial(&mut self, y: f32, cx: &mut Context<Self>) {
        if self.lr.terms.is_empty() {
            self.lr_toast(tx("当前没有显示中的重点").into(), cx);
            return;
        }
        let c = self.dial_near();
        let area = self.lr.area.get().map_or((0., 600.), |b| (f32::from(b.top()), f32::from(b.size.height)));
        let cy = (y - area.0).clamp(80., (area.1 - 150.).max(80.));
        self.lr.dial = Some(Dial { c, c0: c, y0: y, cy });
        self.lr.list_open = false;
        cx.notify();
    }

    pub fn lr_jump_term(&mut self, k: usize, cx: &mut Context<Self>) {
        self.lr.dial = None;
        if let Some(t) = self.lr.terms.get(k).cloned() { self.lr_jump(t.ix, true, cx); }
        cx.notify();
    }

    pub fn lr_rotate(&mut self, by: f32, cx: &mut Context<Self>) {
        let n = self.lr.terms.len().saturating_sub(1) as f32;
        if let Some(d) = self.lr.dial.as_mut() { d.c = (d.c + by).clamp(0., n); }
        cx.notify();
    }

    // ----- window-level pointer handling ------------------------------------

    pub(super) fn lr_pointer_move(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let (x, y) = (f32::from(pos.x), f32::from(pos.y));
        if let Some(press) = self.lr.press {
            if press.long {
                let rows = self.lr.knob_rows.borrow();
                let hot = rows.iter().filter(|(_, b)| f32::from(b.left()) - 8. <= x && x <= f32::from(b.right()) + 8.)
                    .min_by(|a, b| (f32::from(a.1.center().y) - y).abs().total_cmp(&(f32::from(b.1.center().y) - y).abs()))
                    .map(|(k, _)| k.clone());
                drop(rows);
                if hot != self.lr.hot { self.lr.hot = hot; cx.notify(); }
            }
            return;
        }
        match self.lr.drag.clone() {
            Some(Drag::Thumb { x0, y0, moved }) => {
                if let Some(d) = self.lr.dial.as_mut() {
                    let n = self.lr.terms.len().saturating_sub(1) as f32;
                    d.c = (d.c0 + (d.y0 - y) / 44.).clamp(0., n);
                } else if x - x0 < -18. {
                    self.lr_open_dial(y, cx);
                } else {
                    let moved = moved || (y - y0).abs() > 3.;
                    self.lr.drag = Some(Drag::Thumb { x0, y0, moved });
                    if moved { if let Some(b) = self.lr.rail.get() { seek_reading_rail(&self.list, b, pos.y); } }
                }
                cx.notify();
            }
            Some(Drag::Bookmark { id, .. }) => { self.lr.drag = Some(Drag::Bookmark { id, y }); cx.notify(); }
            Some(Drag::Toc { y0, top0 }) => {
                let h = self.lr.area.get().map_or(600., |b| f32::from(b.size.height));
                self.lr.toc_top = (top0 + y - y0).clamp(8., (h - 160.).max(8.));
                cx.notify();
            }
            None => {}
        }
    }

    pub(super) fn lr_pointer_up(&mut self, cx: &mut Context<Self>) {
        if let Some(press) = self.lr.press.take() {
            if press.long {
                match self.lr.hot.take() {
                    Some(key) => self.lr_pick(key, cx),
                    None => self.lr.list_open = false,
                }
            } else {
                // A tap only opens (or closes) the list.
                self.lr.list_open = !press.was_open;
            }
            cx.notify();
            return;
        }
        match self.lr.drag.take() {
            Some(Drag::Thumb { .. }) => {
                if let Some(d) = self.lr.dial { self.lr_jump_term(d.c.round() as usize, cx); }
            }
            Some(Drag::Bookmark { id, y }) => self.lr_drop_bookmark(&id, y, cx),
            _ => {}
        }
        cx.notify();
    }

    pub fn lr_pick(&mut self, key: KnobKey, cx: &mut Context<Self>) {
        self.lr.list_open = false;
        self.lr.hot = None;
        match key {
            KnobKey::Stop(s) => {
                let rows = self.lr_rows().len();
                let here = self.knob_position(rows);
                let far = Self::knob_stops(rows).iter().all(|v| (v - here).abs() >= 0.5);
                self.lr_go_stop(s, far, cx);
            }
            KnobKey::Last => self.lr_go_last(cx),
            KnobKey::Bookmark(id) => {
                let rows = self.lr_rows();
                if let Some((_, ix)) = self.lr_bookmarks(&rows).into_iter().find(|(b, _)| b.id == id) { self.lr_jump(ix, true, cx); }
            }
        }
        cx.notify();
    }

    pub fn lr_knob_press(&mut self, y: f32, cx: &mut Context<Self>) {
        self.lr.dial = None;
        self.lr.knobs_awake = true;
        self.lr.press = Some(Press { y0: y, long: false, was_open: self.lr.list_open });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(220)).await;
            this.update(cx, |this, cx| {
                if let Some(p) = this.lr.press.as_mut() { p.long = true; this.lr.list_open = true; this.lr.hot = None; cx.notify(); }
            }).ok();
        }).detach();
        cx.notify();
    }

    // ----- rendering ----------------------------------------------------------

    /// ‹ and ≡ 目录 before the mode switch.
    pub(super) fn lr_lead_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let in_book = self.model.book().is_some();
        let toc_on = if in_book { self.toc_open } else { self.lr.toc_open };
        h_flex().gap_1()
            .child(Button::new("reader-back").small().ghost().label("‹").tooltip(tx("关闭这篇，回到列表"))
                .on_click(cx.listener(|r, _, _, cx| if let Some(i) = r.model.active() { r.close(i, cx) })))
            .child(Button::new("reader-toc").small().label(tx("≡ 目录")).when(toc_on, |b| b.primary()).when(!toc_on, |b| b.outline())
                .on_click(cx.listener(move |r, _, _, cx| if in_book { r.toggle_toc(cx) } else { r.set_toc(!r.lr.toc_open, cx) })))
            .into_any_element()
    }

    pub fn set_toc(&mut self, open: bool, cx: &mut Context<Self>) {
        self.lr.toc_open = open;
        if !open { self.lr.toc_docked = false; self.lr.toc_pinned = false; }
        cx.notify();
    }

    pub fn set_toc_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) {
        self.lr.toc_pinned = pinned;
        self.lr_toast(tx(if pinned { "已钉住：跳转后不关闭，按住标题可上下拖动" } else { "已取消钉住" }).into(), cx);
    }

    pub fn set_toc_docked(&mut self, docked: bool, cx: &mut Context<Self>) {
        self.lr.toc_docked = docked;
        if docked { self.lr.toc_pinned = true; }
        self.lr_toast(tx(if docked { "目录常驻左侧" } else { "目录回到浮窗" }).into(), cx);
    }

    /// 重点 · 全部 and its menu.
    pub(super) fn lr_filter_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let on = self.lr.filter != Filter::Off;
        let probe = self.lr.menu_anchor.clone();
        div().relative()
            .child(Button::new("highlight-filter").small().label(format!("{} · {}", tx("重点"), tx(self.lr.filter.short())))
                .when(on, |b| b.primary()).when(!on, |b| b.outline())
                .on_click(cx.listener(|r, _, _, cx| { r.lr.menu_open = !r.lr.menu_open; cx.notify(); })))
            .child(canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .into_any_element()
    }

    pub(super) fn lr_filter_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.lr.menu_open { return None; }
        let theme = cx.theme().clone();
        let anchor = self.lr.menu_anchor.get().map_or(point(px(400.), px(200.)), |b| b.bottom_left() + point(px(0.), px(4.)));
        let in_book = self.model.book().is_some();
        let ai = if in_book { 0 } else { self.insights.highlights().map_or(0, <[_]>::len) };
        let mine = if in_book { 0 } else { self.manual_count() };
        let yellow = hsla(45. / 360., 0.85, 0.60, 1.);
        let mut menu = v_flex().w(px(232.)).p_1().gap(px(1.)).rounded(px(12.)).border_1().border_color(theme.border).bg(theme.popover).shadow_md().text_sm()
            .on_mouse_down_out(cx.listener(|r, _, _, cx| { r.lr.menu_open = false; cx.notify(); }));
        for f in Filter::ALL {
            let sw = h_flex().w(px(22.)).gap(px(2.))
                .when(f.shows_ai() && f != Filter::Off, |d| d.child(div().size(px(9.)).rounded(px(2.)).bg(yellow)))
                .when(f.shows_mine() && f != Filter::Off, |d| d.child(div().size(px(9.)).rounded(px(2.)).bg(gold())));
            menu = menu.child(h_flex().id(("hl-opt", f as usize)).gap_2().px_2().py_1p5().rounded(px(7.)).cursor_pointer()
                .when(self.lr.filter == f, |d| d.bg(theme.accent)).hover(|d| d.bg(theme.secondary))
                .child(sw).child(tx(f.option()))
                .when_some(f.count(ai, mine), |d, n| d.child(div().ml_auto().text_xs().text_color(theme.muted_foreground).child(n.to_string())))
                .on_click(cx.listener(move |r, _, _, cx| { r.set_filter(f, cx); r.lr.menu_open = false; })));
        }
        let stale = self.stale_marks();
        if stale > 0 { menu = menu.child(div().px_2().py_1().text_xs().text_color(theme.muted_foreground).child(txf("另有 {0} 条旧版本手工标记，保留但不显示", &[&stale]))); }
        if !in_book && self.insights.available(InsightKind::Highlights) {
            let state = self.insights.state(InsightKind::Highlights);
            let gen = |text: String, action: Option<&'static str>, cx: &mut Context<Self>| v_flex().mx_1p5().my_1().p_2().gap_1p5().rounded(px(8.)).bg(theme.accent).text_xs().text_color(theme.muted_foreground)
                .child(text)
                .when_some(action, |d, label| d.child(Button::new("generate-highlights").xsmall().primary().label(tx(label))
                    .on_click(cx.listener(|r, _, _, cx| r.retry_insight(InsightKind::Highlights, cx)))));
            match state {
                ButtonState::Done => {}
                ButtonState::Generating => menu = menu.child(gen(tx("正在生成 AI 重点…").into(), None, cx)),
                ButtonState::NotGenerated => menu = menu.child(gen(format!("{}\n{}", tx("这篇还没有 AI 重点。"), tx("使用摘要模型，长文可能分多次请求计费")), Some("生成 AI 重点"), cx)),
                ButtonState::Failed(why) | ButtonState::Stale(why) => menu = menu.child(gen(format!("{why} · {}", tx("使用摘要模型，长文可能分多次请求计费")), Some("重新生成"), cx)),
            }
        }
        if !self.lr.marks_supported && !in_book { menu = menu.child(div().px_2().py_1().text_xs().text_color(theme.muted_foreground).child(txf("这台 Mac 的 {0} 还不能保存手工标记，标记只在本机暂存", &[&memex_client::BRAND_NAME]))); }
        Some(deferred(anchored().position(anchor).snap_to_window().child(menu)).with_priority(1).into_any_element())
    }

    /// The selection 浮动条 above the selected text.
    pub(super) fn lr_selection_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.editing.visible || !self.has_selection() { return None; }
        let at = {
            let parts = self.selected.borrow();
            parts.values().filter(|p| !p.text.trim().is_empty()).min_by_key(|p| p.order).and_then(|p| p.at)?
        };
        let target = self.selection_target();
        let why = target.as_ref().err().filter(|w| !w.is_empty()).cloned();
        let ink = hsla(36. / 360., 0.30, 0.95, 1.);
        let mut bar = h_flex().gap(px(2.)).p_1().rounded(px(11.)).bg(hsla(25. / 360., 0.14, 0.15, 1.)).shadow_md().items_center();
        for style in MarkStyle::ALL {
            let glyph = match style {
                MarkStyle::Bold => div().font_weight(FontWeight::BOLD).child("B"),
                MarkStyle::Red => div().text_color(hsla(352. / 360., 0.60, 0.62, 1.)).child(tx("红")),
                MarkStyle::Blue => div().text_color(hsla(215. / 360., 0.60, 0.72, 1.)).child(tx("蓝")),
                MarkStyle::Underline => div().border_b_2().border_color(gold()).child("U"),
                MarkStyle::Yellow => div().px_1().rounded(px(3.)).bg(hsla(45. / 360., 0.85, 0.60, 1.)).text_color(hsla(25. / 360., 0.25, 0.14, 1.)).child(tx("黄")),
            };
            let enabled = why.is_none();
            bar = bar.child(v_flex().id(("mark", style as usize)).min_w(px(34.)).h(px(38.)).items_center().justify_center().rounded(px(8.)).text_color(ink).text_sm()
                .when(enabled, |d| d.cursor_pointer().hover(|d| d.bg(white().opacity(0.14))))
                .when(!enabled, |d| d.opacity(0.4))
                .child(glyph).child(div().text_size(px(9.)).opacity(0.6).child(nav::shortcut(style)))
                .tooltip(move |window, cx| Tooltip::new(format!("{} {}", tx(style.label()), nav::shortcut(style))).build(window, cx))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .when(enabled, |d| d.on_click(cx.listener(move |r, _, window, cx| r.apply_mark(style, window, cx)))));
        }
        bar = bar.child(div().w(px(1.)).h(px(26.)).mx_0p5().bg(white().opacity(0.2)))
            .child(div().id("mark-ai").px_2().h(px(38.)).flex().items_center().rounded(px(8.)).bg(cx.theme().primary).text_color(white()).text_sm().cursor_pointer().child("AI")
                .tooltip(|window, cx| Tooltip::new(tx("带着选中文字问 AI（放进对话框，点发送才提问）")).build(window, cx))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(cx.listener(|r, _, window, cx| r.ask_about_selection(window, cx))));
        let el = v_flex().items_start().gap_1().child(bar)
            .when_some(why, |d, w| d.child(div().px_2().py_0p5().rounded(px(6.)).bg(hsla(25. / 360., 0.14, 0.15, 0.92)).text_xs().text_color(ink).child(w)));
        let pos = point(at.x - px(120.), at.y - px(56.));
        Some(deferred(anchored().position(pos).snap_to_window().child(el)).with_priority(1).into_any_element())
    }

    /// The overlays of the reading area: rail, dial, knobs and their list,
    /// speaker bubble, floating 目录, bookmark ribbons and the toast.
    pub(super) fn lr_overlays(&self, rows: &[LRow], p: &Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let reader = cx.entity().downgrade();
        let area = self.lr.area.clone();
        let mut out = vec![];
        // Window-level move / up while something is dragged or pressed.
        let r1 = reader.clone();
        out.push(canvas(move |b, _, _| area.set(Some(b)), move |_, _, window, _| {
            let r = r1.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase.bubble() { r.update(cx, |this, cx| if this.lr.drag.is_some() || this.lr.press.is_some() { this.lr_pointer_move(e.position, cx) }).ok(); }
            });
            let r = r1.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                if phase.bubble() && e.button == MouseButton::Left { r.update(cx, |this, cx| if this.lr.drag.is_some() || this.lr.press.is_some() { this.lr_pointer_up(cx) }).ok(); }
            });
        }).absolute().top_0().left_0().size_full().into_any_element());
        let total = rows.len();
        let bookmarks = self.lr_bookmarks(rows);
        // Bookmark ribbons at their paragraph's start, draggable.
        if let Some(a) = self.lr.area.get() {
            for (b, ix) in &bookmarks {
                let dragging = matches!(&self.lr.drag, Some(Drag::Bookmark { id, .. }) if *id == b.id);
                let y = match &self.lr.drag { Some(Drag::Bookmark { id, y }) if *id == b.id => Some(*y), _ => self.list.bounds_for_item(*ix).map(|r| f32::from(r.top()) + 2.) };
                let Some(y) = y else { continue };
                let top = y - f32::from(a.top());
                if top < -20. || top > f32::from(a.size.height) { continue; }
                let id = b.id.clone();
                out.push(div().id(SharedString::from(format!("bm-{}", b.id))).absolute().left(px(-18.)).top(px(top)).w(px(13.)).h(px(18.)).cursor(CursorStyle::ResizeUpDown)
                    .child(ribbon(p.rose, dragging))
                    .tooltip(|window, cx| Tooltip::new(tx("书签：按住上下拖动到其他段落")).build(window, cx))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |r, e: &MouseDownEvent, _, cx| { r.lr.drag = Some(Drag::Bookmark { id: id.clone(), y: f32::from(e.position.y) }); cx.stop_propagation(); cx.notify(); }))
                    .into_any_element());
            }
        }
        // Speaker bubble once the current speaker's name has scrolled away.
        if let Some((name, unavailable)) = self.lr_speaker(rows) {
            out.push(speaker_bubble(name, unavailable, p, &theme));
        }
        // The rail.
        out.push(self.lr_rail(total, &bookmarks, p, cx));
        // The dial.
        if let Some(d) = self.lr.dial { out.push(self.lr_dial(d, cx)); }
        // Knobs and their list.
        out.push(self.lr_knobs(dark, cx));
        if self.lr.list_open { out.push(self.lr_knob_list(total, &bookmarks, cx)); }
        // Floating 目录 (docked: drawn beside the text instead).
        if self.lr.toc_open && !self.lr.toc_docked && self.model.book().is_none() { out.push(self.lr_toc(false, rows, cx)); }
        if let Some(t) = &self.lr.toast {
            out.push(div().absolute().bottom(px(12.)).left_0().right_0().flex().justify_center()
                .child(div().px_2p5().py_1().rounded_full().bg(theme.foreground).text_color(theme.background).text_xs().child(t.clone())).into_any_element());
        }
        out
    }

    /// The speaker of the paragraph being read, as the 说话人名字 setting shows
    /// it, and whether its reference name is unavailable.
    fn lr_speaker(&self, rows: &[LRow]) -> Option<(String, bool)> {
        let top = self.top_ix().checked_sub(LEAD)?;
        let offset = f32::from(self.list.logical_scroll_top().offset_in_item);
        let (blocks, translation) = (self.model.original(), self.model.translation());
        let mode = memex_client::speaker_names();
        for row in (0..=top.min(rows.len().saturating_sub(1))).rev() {
            // 译文 rows (only the translation is shown): its own labels, named per the setting.
            if let Some(LRow::Translation(i)) = rows.get(row) {
                let Some(b) = translation.get(*i) else { continue };
                if matches!(b.kind, BlockKind::Heading(_)) { return None; }
                let speakers = self.translated_speakers();
                if let Some(name) = speakers.shown_t(&b.text, mode) {
                    let unavailable = mode != memex_client::SpeakerNames::Translated && speakers.unmapped_t(&b.text);
                    return (row < top || offset > 14.).then_some((name, unavailable));
                }
                continue;
            }
            let Some(b) = Self::row_block(rows, row).and_then(|i| blocks.get(i)) else { continue };
            if matches!(b.kind, BlockKind::Heading(_)) { return None; }
            if let Some(name) = nav::speaker(&b.text) {
                // The original side, under its Chinese reference when that is the choice.
                let speakers = self.translated_speakers();
                let unavailable = mode != memex_client::SpeakerNames::Original && speakers.unmapped_o(name);
                return (row < top || offset > 14.).then(|| (speakers.shown_o(name, mode), unavailable));
            }
        }
        None
    }

    /// The speaker bubble of the AI translation column (only the AI text shown).
    pub(super) fn ai_speaker(&self, blocks: &[Block]) -> Option<(String, bool)> {
        let top = self.list_t.logical_scroll_top();
        let offset = f32::from(top.offset_in_item);
        let mode = memex_client::speaker_names();
        let speakers = self.translated_speakers();
        for i in (0..=top.item_ix.min(blocks.len().saturating_sub(1))).rev() {
            let b = &blocks[i];
            if matches!(b.kind, BlockKind::Heading(_)) { return None; }
            if let Some(name) = speakers.shown_t(&b.text, mode) {
                let unavailable = mode != memex_client::SpeakerNames::Translated && speakers.unmapped_t(&b.text);
                return (i < top.item_ix || offset > 14.).then_some((name, unavailable));
            }
        }
        None
    }

    /// Speakers of the translation on screen and their reference names, from
    /// the recorded one-to-one block correspondence already loaded for 对照
    /// (the AI text's `translate_alignment`, or a book chapter's paragraph
    /// index). An ordinary item has none: its labels stay as written.
    pub(super) fn translated_speakers(&self) -> Rc<nav::TranslatedSpeakers> {
        use memex_client::book::Relation;
        let original = self.model.original();
        let ai = self.ai_text().map(|(m, _, b)| (m.version.clone(), b));
        let (translation, rows): (&[Block], &[crate::model::AlignedRow]) = match &ai {
            Some((_, b)) => (b, self.ai_rows().unwrap_or(&[])),
            None if self.model.book().is_some() => (self.model.translation(), self.model.aligned()),
            None => (self.model.translation(), &[]),
        };
        let key = format!("{:?}|{:?}|{}|{}|{}|{}", self.model.detail().map(|d| &d.canonical.revision_id), ai.as_ref().map(|a| &a.0), self.model.book().map_or(0, |b| b.at),
            original.len(), translation.len(), rows.len());
        if let Some((k, s)) = self.lr.speakers_t.borrow().as_ref() {
            if *k == key { return s.clone(); }
        }
        let one = |r: &std::ops::Range<usize>| r.end == r.start + 1;
        let pairs: Vec<(&str, &str)> = rows.iter()
            .filter(|r| r.relation == Relation::Matched && one(&r.o) && one(&r.t))
            .filter_map(|r| Some((original.get(r.o.start)?.text.as_str(), translation.get(r.t.start)?.text.as_str())))
            .collect();
        let paragraphs = translation.iter().filter(|b| b.kind == BlockKind::Paragraph).map(|b| b.text.as_str());
        let s = Rc::new(nav::TranslatedSpeakers::from_pairs(pairs, paragraphs));
        *self.lr.speakers_t.borrow_mut() = Some((key, s.clone()));
        s
    }

    fn lr_rail(&self, total: usize, bookmarks: &[(Bookmark, usize)], p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let frac = self.reading_pct as f32 / 100.;
        let track = if theme.background.l < 0.5 { hsla(25. / 360., 0.08, 0.30, 1.) } else { hsla(30. / 360., 0.14, 0.84, 1.) };
        let thumb = theme.muted_foreground.opacity(0.4);
        let ticks: Vec<(f32, Hsla, bool)> = if total == 0 { vec![] } else { self.lr.terms.iter().map(|t| (((t.ix - LEAD) as f32 + 0.5) / total as f32, t.color, t.manual)).collect() };
        let marks: Vec<f32> = bookmarks.iter().map(|(_, ix)| ((ix - LEAD) as f32) / total.max(1) as f32).collect();
        let rose = p.rose;
        let probe = self.lr.rail.clone();
        let h = self.lr.rail.get().map_or(400., |b| f32::from(b.size.height));
        let t0 = frac * (h - TH).max(0.);
        let low = t0 + TH > h - 150.;
        div().id("lr-rail").absolute().top(px(8.)).bottom(px(8.)).right(px(0.)).w(px(38.)).cursor_pointer()
            .child(canvas(move |b, _, _| { probe.set(Some(b)); b }, move |b, _, window, _| {
                let (h, left, top) = (f32::from(b.size.height), f32::from(b.left()), f32::from(b.top()));
                let t0 = frac * (h - TH).max(0.);
                let t1 = t0 + TH;
                let bump = |y: f32| -> f32 {
                    if y >= t0 && y <= t1 { return 1.; }
                    let d = if y < t0 { t0 - y } else { y - t1 };
                    if d >= TR { return 0.; }
                    let u = 1. - d / TR;
                    u * u * (3. - 2. * u)
                };
                let x_at = |y: f32| BX - BUMP * bump(y);
                let mut y = 0.;
                while y < h {
                    let x = x_at(y);
                    window.paint_quad(fill(Bounds::new(point(px(left + x - 2.5), px(top + y)), size(px(5.), px(1.))), track));
                    if y >= t0 - TR && y <= t1 + TR && x + 3.5 < BX + 2.5 {
                        window.paint_quad(fill(Bounds::new(point(px(left + x + 3.5), px(top + y)), size(px(BX + 2.5 - x - 3.5), px(1.))), thumb));
                    }
                    y += 1.;
                }
                for (f, color, manual) in &ticks {
                    let yy = f * h;
                    let x = x_at(yy);
                    let (w, hh) = if *manual { (7., 3.) } else { (5., 2.) };
                    window.paint_quad(fill(Bounds::new(point(px(left + x - w / 2.), px(top + yy - hh / 2.)), size(px(w), px(hh))), *color).corner_radii(px(1.5)));
                }
                for f in &marks {
                    let yy = f * h;
                    let x = x_at(yy);
                    window.paint_quad(fill(Bounds::new(point(px(left + x - 9.), px(top + yy - 4.)), size(px(5.), px(8.))), rose));
                }
            }).absolute().top_0().left_0().size_full())
            .child(div().absolute().top(px(if low { t0 - 16. } else { t0 + 9. })).right(px(if low { 4. } else { 38. - (BX - BUMP) + 12. })).whitespace_nowrap().text_size(px(10.)).text_color(theme.muted_foreground).child(format!("{}%", self.reading_pct)))
            .tooltip(|window, cx| Tooltip::new(tx("拖动滑钮；往左拉打开重点转盘；双击加书签")).build(window, cx))
            .on_mouse_down(MouseButton::Left, cx.listener(move |r, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let Some(b) = r.lr.rail.get() else { return };
                let y = f32::from(e.position.y) - f32::from(b.top());
                let h = f32::from(b.size.height);
                let t0 = (r.reading_pct as f32 / 100.) * (h - TH).max(0.);
                let on_thumb = y >= t0 - 6. && y <= t0 + TH + 6.;
                if on_thumb && e.click_count >= 2 { r.lr.drag = None; r.lr_add_bookmark(cx); return; }
                if !on_thumb { seek_reading_rail(&r.list, b, e.position.y); }
                r.lr.drag = Some(Drag::Thumb { x0: f32::from(e.position.x), y0: f32::from(e.position.y), moved: false });
                cx.notify();
            }))
            .into_any_element()
    }

    fn lr_dial(&self, d: Dial, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let terms = self.lr.terms.clone();
        let h = self.lr.area.get().map_or(600., |b| f32::from(b.size.height));
        let mut el = div().id("lr-dial").absolute().top_0().left_0().right_0().bottom_0().bg(theme.background.opacity(0.93)).overflow_hidden()
            .on_click(cx.listener(|r, _, _, cx| { r.lr.dial = None; cx.notify(); }))
            .on_scroll(cx.listener(|r, e: &ScrollEvent, _, cx| { let by = -f32::from(e.delta.pixel_delta(px(20.)).y) / 44.; r.lr_rotate(by, cx); cx.stop_propagation(); }))
            .child(div().absolute().left(px(14.)).top(px(10.)).text_xs().text_color(theme.muted_foreground)
                .child(txf("重点 {0} / {1}", &[&(d.c.round() as usize + 1), &terms.len()])));
        for (k, t) in terms.iter().enumerate() {
            let o = k as f32 - d.c;
            let a = o.abs();
            if a > 5.5 { continue; }
            let top = d.cy + o * 46. - 15.;
            if top < 30. || top > h - 36. { continue; }
            let center = a < 0.5;
            let (bg, border, fg, muted) = match (t.manual, center) {
                (true, true) => (gold(), gold(), white(), white().opacity(0.8)),
                (true, false) => (gold_wash(dark), gold(), if dark { hsla(36. / 360., 0.80, 0.70, 1.) } else { hsla(28. / 360., 0.75, 0.34, 1.) }, theme.muted_foreground),
                (false, true) => (theme.primary, theme.primary, theme.primary_foreground, theme.primary_foreground.opacity(0.78)),
                (false, false) => (theme.background, theme.border, theme.foreground, theme.muted_foreground),
            };
            let tag = if t.manual {
                div().flex_none().px_1().rounded(px(4.)).text_size(px(10.)).bg(if center { white() } else { gold() }).text_color(if center { gold() } else { white() }).child(tx("手工"))
            } else {
                div().flex_none().px_1().rounded(px(4.)).text_size(px(10.)).border_1().border_color(if center { theme.primary_foreground.opacity(0.5) } else { theme.border }).text_color(muted).child("AI")
            };
            el = el.child(h_flex().id(("dial-card", k)).absolute().top(px(top)).right(px(46. + a * a * 2.4)).max_w(px(340.)).gap_1p5().items_center()
                .pl(px(if t.manual { 10. } else { 14. })).pr_3().py_1p5().rounded(px(10.)).bg(bg).border_1().border_color(border)
                .when(t.manual, |d| d.border_2()).opacity(1. - a * 0.06).text_size(px(12. - a * 0.3)).cursor_pointer()
                .when(!t.manual && !center, |d| d.child(div().absolute().left_0().top(px(4.)).bottom(px(4.)).w(px(3.)).rounded(px(2.)).bg(t.color)))
                .child(tag)
                .child(div().min_w_0().truncate().text_color(muted).child(t.ctx.clone()))
                .child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(fg).child(t.kw.clone()))
                .child(div().flex_none().text_size(px(10.)).text_color(muted).child(format!("{}%", t.pct)))
                .on_click(cx.listener(move |r, _, _, cx| { cx.stop_propagation(); r.lr_jump_term(k, cx); })));
        }
        el.into_any_element()
    }

    /// The phone's knobs (app.css `.knobs`): a raised metal rim, a concave cap,
    /// the middle one larger with a rose ring; .38 at rest, whole when used.
    /// GPUI's element opacity fades each layer on its own (rim, cap, shadows
    /// then show through one another as a grey double image), so the rest state
    /// mixes every colour into the page instead and each layer stays opaque.
    fn lr_knobs(&self, dark: bool, cx: &mut Context<Self>) -> AnyElement {
        let awake = self.lr.knobs_awake || self.lr.list_open || self.lr.press.is_some();
        let pressed = self.lr.press.is_some();
        let (page, rose) = (cx.theme().background, cx.theme().primary);
        let k = if awake { 1. } else { 0.38 };
        let solid = move |c: Hsla| if awake { c } else { page.blend(c.opacity(k)) };
        let shade = move |a: f32| black().opacity(a * k);
        let shine = move |a: f32| white().opacity(a * k);
        let g = |l: f32, s: f32| hsla(30. / 360., s, l, 1.);
        // Rim light → dark (lit from the top left), cap light at the bottom (concave), ring, glyph.
        let (rim, cap, ring, ink) = if dark {
            ((g(0.92, 0.06), g(0.60, 0.05)), (g(0.92, 0.), g(0.74, 0.05)), g(0.62, 0.05), g(0.34, 0.08))
        } else {
            ((g(0.99, 0.), g(0.72, 0.07)), (g(0.98, 0.), g(0.83, 0.06)), g(0.74, 0.08), g(0.45, 0.08))
        };
        let rose_ring = solid(rim.1.blend(rose.opacity(0.7)));
        let knob = move |id: &'static str, size: f32, glyph: Option<&'static str>, mid: bool, down: bool| {
            let inset = if mid { 8. } else { 7. };
            let drop = if down {
                BoxShadow::new(px(0.), px(1.), shade(0.40)).blur_radius(px(2.))
            } else {
                BoxShadow::new(px(0.), px(3.), shade(0.40)).blur_radius(px(6.)).spread_radius(px(-2.))
            };
            div().id(id).relative().flex_none().size(px(size)).rounded_full().cursor_pointer()
                .bg(linear_gradient(150., linear_color_stop(solid(rim.0), 0.), linear_color_stop(solid(rim.1), 1.)))
                .shadow(vec![
                    drop,
                    BoxShadow::new(px(0.), px(0.), solid(rim.1.blend(black().opacity(0.12)))).spread_radius(px(0.5)),
                    BoxShadow::new(px(0.), px(-2.), shade(0.18)).blur_radius(px(3.)).inset(),
                    BoxShadow::new(px(0.), px(2.), shine(0.8)).blur_radius(px(2.)).inset(),
                ])
                .child(div().absolute().top(px(inset)).left(px(inset)).size(px(size - inset * 2.)).rounded_full()
                    .bg(linear_gradient(0., linear_color_stop(solid(cap.0), 0.), linear_color_stop(solid(cap.1), 1.)))
                    .shadow(vec![
                        BoxShadow::new(px(0.), px(0.), if mid { rose_ring } else { solid(ring) }).spread_radius(px(if mid { 1.5 } else { 1. })),
                        BoxShadow::new(px(0.), px(2.), shade(if mid { 0.25 } else { 0.22 })).blur_radius(px(3.)).inset(),
                        BoxShadow::new(px(0.), px(-1.), shine(0.9)).blur_radius(px(1.)).inset(),
                    ])
                    .flex().items_center().justify_center().text_size(px(9.)).line_height(px(9.)).text_color(solid(ink))
                    .when_some(glyph, |d, g| d.child(g))
                    .when(mid, |d| d.child(div().size(px(6.)).rounded_full().border_1().border_color(solid(cap.1.blend(rose.opacity(0.6)))))))
        };
        v_flex().id("lr-knobs").absolute().right(px(KNOB_AXIS - 22.)).bottom(px(54.)).w(px(44.)).gap(px(10.)).items_center()
            .on_hover(cx.listener(|r, h: &bool, _, cx| { r.lr.knobs_awake = *h; cx.notify(); }))
            .child(knob("knob-up", 34., Some("▲"), false, false).tooltip(|window, cx| Tooltip::new(tx("上一个位置：纪要 / 正文开头 / 正文结尾 / AI 对话记录")).build(window, cx))
                .on_click(cx.listener(|r, _, _, cx| r.lr_step(true, cx))))
            .child(knob("knob-mid", 44., None, true, pressed)
                .tooltip(|window, cx| Tooltip::new(tx("按住选择跳转位置；轻点展开列表")).build(window, cx))
                .on_mouse_down(MouseButton::Left, cx.listener(|r, e: &MouseDownEvent, _, cx| { cx.stop_propagation(); r.lr_knob_press(f32::from(e.position.y), cx); })))
            .child(knob("knob-down", 34., Some("▼"), false, false).tooltip(|window, cx| Tooltip::new(tx("下一个位置：纪要 / 正文开头 / 正文结尾 / AI 对话记录")).build(window, cx))
                .on_click(cx.listener(|r, _, _, cx| r.lr_step(false, cx))))
            .into_any_element()
    }

    fn lr_knob_list(&self, total: usize, bookmarks: &[(Bookmark, usize)], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let rows = self.lr.knob_rows.clone();
        rows.borrow_mut().clear();
        let pct = |ix: usize| nav::percent(ix.saturating_sub(LEAD), total);
        let item = |key: KnobKey, glyph: AnyElement, label: String, n: Option<String>, enabled: bool, wash: Hsla, cx: &mut Context<Self>| {
            let hot = self.lr.hot.as_ref() == Some(&key);
            let probe = rows.clone();
            let k2 = key.clone();
            h_flex().id(SharedString::from(format!("knob-item-{key:?}"))).relative().gap_1p5().px_1p5().py_1().rounded(px(6.)).min_h(px(26.)).text_sm()
                .when(hot, |d| d.bg(wash)).when(enabled, |d| d.cursor_pointer().hover(|d| d.bg(wash))).when(!enabled, |d| d.text_color(theme.muted_foreground))
                .child(glyph).child(div().truncate().child(label))
                .when_some(n, |d, n| d.child(div().ml_auto().text_xs().text_color(theme.muted_foreground).child(n)))
                .child(canvas(move |b, _, _| probe.borrow_mut().push((k2.clone(), b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                .when(enabled, |d| d.on_click(cx.listener(move |r, _, _, cx| r.lr_pick(key.clone(), cx))))
        };
        let g = |s: &'static str| div().w(px(12.)).flex_none().text_xs().text_color(theme.primary).child(s).into_any_element();
        let col = || v_flex().w(px(150.)).p_1().gap(px(1.)).rounded(px(12.)).border_1().border_color(theme.border).bg(theme.popover).shadow_md();
        let chat_n = self.lr.chat.len();
        let c1 = col()
            .child(item(KnobKey::Stop(Stop::Minutes), g("◆"), tx("纪要").into(), None, true, theme.accent, cx))
            .child(item(KnobKey::Stop(Stop::Start), g("▲"), tx("正文开头").into(), None, true, theme.accent, cx))
            .child(item(KnobKey::Last, g("↺"), tx("上次读到").into(), Some(self.lr.last_read.map_or("—".into(), |ix| format!("{}%", pct(ix)))), self.lr.last_read.is_some(), theme.accent, cx))
            .child(item(KnobKey::Stop(Stop::End), g("▼"), tx("正文结尾").into(), None, true, theme.accent, cx))
            .child(item(KnobKey::Stop(Stop::Chat), g("✦"), tx("AI 对话记录").into(), Some(chat_n.to_string()), true, theme.accent, cx));
        let c2 = (!bookmarks.is_empty()).then(|| {
            let mut c = col().child(div().px_1p5().py_0p5().text_xs().text_color(theme.muted_foreground).child(format!("{} · {}", tx("书签"), bookmarks.len())));
            for (k, (b, ix)) in bookmarks.iter().enumerate() {
                c = c.child(item(KnobKey::Bookmark(b.id.clone()), ribbon(theme.primary, false).w(px(8.)).h(px(11.)).into_any_element(), format!("{} {}", tx("书签"), k + 1), Some(format!("{}%", pct(*ix))), true, gold_wash(dark), cx));
            }
            c
        });
        h_flex().absolute().right(px(102.)).bottom(px(54.)).gap_1p5().items_end()
            .on_mouse_down_out(cx.listener(|r, _, _, cx| if r.lr.press.is_none() { r.lr.list_open = false; cx.notify(); }))
            .children(c2).child(c1).into_any_element()
    }

    /// 目录 of headings and timestamps: floating (`docked` false) or 常驻.
    pub(super) fn lr_toc(&self, docked: bool, rows: &[LRow], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let blocks = self.model.original();
        let mut entries: Vec<(usize, Option<String>, String)> = vec![];
        for (row, _) in rows.iter().enumerate() {
            let Some(b) = Self::row_block(rows, row).and_then(|i| blocks.get(i)) else { continue };
            match b.kind {
                BlockKind::Heading(_) => {
                    let title = b.text[..b.heading_len.min(b.text.len())].trim().to_string();
                    match nav::timestamp(&title) { Some((t, rest)) => entries.push((row, Some(t), rest)), None => entries.push((row, None, title)) }
                }
                _ => if let Some((t, rest)) = nav::timestamp(&b.text) { entries.push((row, Some(t), rest)); },
            }
        }
        let top = self.top_ix().saturating_sub(LEAD);
        let current = entries.iter().rposition(|e| e.0 <= top + 1);
        let pinned = self.lr.toc_pinned;
        let head = h_flex().id("toc-head").gap_1().items_center().pl_1p5().pr_0p5().pb_1().text_xs().text_color(theme.muted_foreground)
            .when(pinned && !docked, |d| d.cursor(CursorStyle::ResizeUpDown).child(div().child("⋮⋮"))
                .on_mouse_down(MouseButton::Left, cx.listener(|r, e: &MouseDownEvent, _, cx| { r.lr.drag = Some(Drag::Toc { y0: f32::from(e.position.y), top0: r.lr.toc_top }); cx.notify(); })))
            .child(div().mr_auto().child(tx("目录")))
            .when(!docked, |d| d.child(Button::new("toc-pin").xsmall().label(tx(if pinned { "已钉住" } else { "钉住" })).when(pinned, |b| b.primary()).when(!pinned, |b| b.outline())
                .on_click(cx.listener(move |r, _, _, cx| r.set_toc_pinned(!pinned, cx)))))
            .when(pinned || docked, |d| d.child(Button::new("toc-dock").xsmall().label(tx(if docked { "取消常驻" } else { "常驻" })).when(docked, |b| b.primary()).when(!docked, |b| b.outline())
                .on_click(cx.listener(move |r, _, _, cx| r.set_toc_docked(!docked, cx)))));
        let mut list = v_flex().gap(px(1.)).text_sm();
        if entries.is_empty() { list = list.child(div().px_2().py_1().text_xs().text_color(theme.muted_foreground).child(tx("这篇没有标题或时间标注"))); }
        for (k, (row, time, title)) in entries.into_iter().enumerate() {
            let ix = LEAD + row;
            list = list.child(h_flex().id(("toc-entry", k)).gap_1p5().px_2().py_1().rounded(px(6.)).cursor_pointer()
                .when(current == Some(k), |d| d.bg(theme.accent)).hover(|d| d.bg(theme.secondary))
                .when_some(time, |d, t| d.child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(t)))
                .child(div().truncate().child(title))
                .on_click(cx.listener(move |r, _, _, cx| { r.lr_jump(ix, true, cx); if !r.lr.toc_pinned && !r.lr.toc_docked { r.lr.toc_open = false; } })));
        }
        let body = v_flex().id("lr-toc").p_1p5().gap_0p5().bg(theme.popover).child(head).child(list.id("toc-list").max_h(px(420.)).overflow_y_scroll())
            .child(div().px_2().pt_1().text_xs().text_color(theme.muted_foreground).child(tx("按原文标题与时间标注生成")));
        if docked {
            body.w(px(200.)).h_full().flex_none().border_r_1().border_color(theme.border).into_any_element()
        } else {
            body.absolute().top(px(self.lr.toc_top)).left(px(4.)).w(px(236.)).rounded(px(12.)).border_1().border_color(theme.border).shadow_md()
                .on_mouse_down_out(cx.listener(|r, _, _, cx| if !r.lr.toc_pinned && r.lr.drag.is_none() { r.lr.toc_open = false; cx.notify(); }))
                .into_any_element()
        }
    }

    /// After the last row: 正文结尾 and the stored AI conversation (read-only).
    pub(super) fn lr_end_zone(chat: &[StoredTurn], p: &Palette, card: Option<AnyElement>) -> AnyElement {
        let mut zone = v_flex().gap_1().px_3().py_2p5().mb_6().rounded(px(10.)).bg(p.accent.opacity(0.08)).text_sm().line_height(relative(1.6))
            .child(div().text_xs().text_color(p.muted).child(format!("{} · {}", tx("AI 对话记录"), chat.len())));
        if chat.is_empty() { zone = zone.child(div().text_color(p.muted).child(tx("还没有 AI 对话记录"))); }
        for t in chat.iter().take(40) {
            let who = if t.role == ChatRole::User { tx("问：") } else { tx("答：") };
            zone = zone.child(div().text_color(p.fg).child(format!("{who}{}", t.content)));
        }
        v_flex().pt_4()
            .child(div().mb_2().pt_1p5().border_t_1().border_dashed().border_color(p.muted.opacity(0.4)).text_xs().text_color(p.muted).child(tx("正文结尾")))
            .children(card)
            .child(zone).into_any_element()
    }

    /// AI spans of the shown side(s) are drawn only as the switch allows; manual ones likewise.
    pub(super) fn lr_shows(&self) -> (bool, bool) {
        (self.lr.filter.shows_ai(), self.lr.filter.shows_mine())
    }

    // ----- development walk accessors -------------------------------------------

    pub fn dev_longread(&self) -> String {
        format!(
            "filter={:?} terms={} manual={} dial={:?} list_open={} last={:?} toc={}/{}/{} state={} bookmarks={} marks={:?} supported={} pending={} notice={:?} mark_notice={:?} chat={} top={}",
            self.lr.filter, self.lr.terms.len(), self.lr.terms.iter().filter(|t| t.manual).count(), self.lr.dial.map(|d| d.c), self.lr.list_open, self.lr.last_read,
            self.lr.toc_open, self.lr.toc_pinned, self.lr.toc_docked, self.lr.state.as_ref().map_or("-".into(), |s| s.version.chars().take(8).collect::<String>()),
            self.lr.state.as_ref().map_or(0, |s| s.state.bookmarks.len()), self.lr.ledger.as_ref().map(|l| (l.version.clone(), l.annotations.len())),
            self.lr.marks_supported, self.lr.pending.is_some(), self.lr.notice, self.lr.mark_notice, self.lr.chat.len(), self.top_ix()
        )
    }
    pub fn dev_open_menu(&mut self, cx: &mut Context<Self>) { self.lr.menu_open = true; cx.notify(); }
    pub fn dev_open_list(&mut self, cx: &mut Context<Self>) { self.lr.list_open = true; self.lr.knobs_awake = true; cx.notify(); }
    pub fn dev_open_dial(&mut self, cx: &mut Context<Self>) {
        let y = self.lr.rail.get().map_or(300., |b| f32::from(b.top()) + f32::from(b.size.height) * self.reading_pct as f32 / 100.);
        self.lr_open_dial(y, cx);
    }
    pub fn dev_toc(&mut self, open: bool, pinned: bool, docked: bool, cx: &mut Context<Self>) {
        self.lr.toc_open = open; self.lr.toc_pinned = pinned; self.lr.toc_docked = docked; cx.notify();
    }
    pub fn dev_scroll_to(&mut self, ix: usize, cx: &mut Context<Self>) { self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(30.) }); cx.notify(); }
    pub fn dev_add_bookmark(&mut self, cx: &mut Context<Self>) { self.lr_add_bookmark(cx); }
    pub fn dev_pending_mark(&self) -> Option<PendingMark> { self.lr.pending.clone() }
    /// Selects body bytes `range` of the original (as the renderer's source map
    /// would report a drag selection) and focuses the reading area.
    pub fn dev_select(&mut self, range: Range<usize>, translation: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.model.detail().and_then(|d| d.canonical.markdown.get(range.clone()).map(str::to_string)).unwrap_or_default();
        self.selected.borrow_mut().clear();
        self.selected.borrow_mut().insert("dev".into(), crate::selection::SelectedPart { source: Some(range), translation, ai: false, text, order: 0, at: Some(point(px(700.), px(400.))) });
        self.focus.focus(window, cx);
        cx.notify();
    }
    pub fn dev_marks(&self) -> (usize, Option<String>, Filter) { (self.manual_count(), self.lr.mark_notice.clone(), self.lr.filter) }
}

fn ribbon(color: Hsla, dragging: bool) -> Div {
    // A bookmark: a narrow rose band with a notch (drawn as a rect and a pale notch).
    div().relative().w(px(13.)).h(px(18.)).bg(color).rounded_t(px(1.)).when(dragging, |d| d.opacity(0.8))
        .child(div().absolute().bottom_0().left(px(4.)).w(px(5.)).h(px(5.)).bg(white().opacity(0.85)).rounded_t_full())
}
