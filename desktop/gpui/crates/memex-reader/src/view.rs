//! Reader view: tab strip, 原文 / 对照 / 译文 switch, 翻译此文, 重点标注 / 金句 /
//! 事实核查, virtualized long text and citation highlight. Owns `ReaderModel`,
//! the translate and insight states and their backend requests. Body text is
//! never italic: emphasis is shown upright, bold stays bold.

use crate::inline::{block_spans, marked_sides, merge, AlongMark, LineKind, Span};
use crate::insights::{ButtonState, Insights, View};
use crate::model::{image_placeholder, Block, BlockKind, JumpOutcome, Mode, ReaderModel, Row};
use crate::translate::{looks_chinese, MinutesAsk, Next, Translate};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    text::TextView,
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use memex_client::insights::{confidence_label, InsightKind, Side, Verdict};
use memex_client::reading_stats::{label as stats_label, reading_percent};
use memex_client::txf;
use std::{collections::HashMap, ops::Range, rc::Rc, time::Duration};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::base::Disableable;
use gpui_kit::*;
use memex_client::{relabel, tx, AiMinutes, Citation, Client, ContextRequest, MinutesState, Sensitivity};
#[path = "editing.rs"]
mod editing;
#[path = "chapter_translate.rs"]
mod chapter_translate;
#[path = "ai_marks.rs"]
mod ai_marks;
#[path = "format.rs"]
mod format_cleanup;
#[path = "longread.rs"]
pub mod longread;
#[path = "passes_ui.rs"]
pub mod passes_ui;
#[path = "cover.rs"]
mod cover;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaderEvent {
    Saved,
    /// The active tab changed to this item (shell syncs library/metadata/chat).
    Activated(Option<String>),
    /// The user picked another reading mode (the shell remembers it).
    ModeChanged(Mode),
    /// "No translation model is configured" → open settings.
    OpenSettings,
    /// A requested translation finished; other views showing its state reload.
    TranslationReady,
    /// The reading typeface was switched (the shell remembers it).
    FontChanged(memex_client::ReadingFont),
    SizeChanged(memex_client::ReadingSize),
    /// Book 对照 switched between 段落 and 并排 (the shell remembers it).
    BookParallelChanged(bool),
    /// An item's 古籍 display (简繁 / 竖排 / typeface) changed (the shell keeps it per item).
    ItemStyleChanged(String, memex_client::ItemStyle),
    /// AI on a selection: open the conversation with this quote prefilled (never sent).
    Ask(String),
    /// ⏱ was switched (the shell remembers the choice; it never creates a pass itself).
    PassTimingChanged(bool),
}

/// The 整理本章段落 request of the chapter on screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Repair {
    #[default]
    Idle,
    Confirm,
    Running { job_id: String, stage: String },
    /// The outcome is unknown (admission lost or job outcome_unknown): 查询原任务 resends the same request id and gets the original job back; nothing is retried automatically.
    Unknown(String),
    /// Ended without success: 再试一次 makes a new request.
    Failed(String),
    Done,
}

/// What a 「Mac 正忙，稍后重试」 line reads again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyRetry {
    /// The item (and its book).
    Item,
    /// Book entry `at`: its chapter texts and paragraph index.
    Chapter(usize),
}

pub struct Reader {
    client: Client,
    reading_size: memex_client::ReadingSize,
    editing: editing::Editing,
    edit_allowed: bool,
    annotations_allowed: bool,
    annotations: Option<memex_client::reading::ReadingAnnotations>,
    annotation_saving: bool,
    annotation_notice: Option<String>,
    selected: crate::selection::Selection,
    images: std::collections::VecDeque<(String, Entity<crate::image::ReaderImage>)>,
    pub model: ReaderModel,
    pub translate: Translate,
    pub insights: Insights,
    ticket: u64,
    /// Invalidates insight requests and polls of an item no longer shown.
    insight_ticket: u64,
    insight_polling: bool,
    /// Label of the AI highlight last clicked (shown in the legend line).
    picked: Option<String>,
    /// AI highlight quotes that could not be placed in their block (last frame).
    skipped_highlights: usize,
    /// 发布日期 and 时长 of each opened item, as its list row reported them
    /// (`published_at`, `duration_seconds`); shown under the title.
    item_dates: HashMap<String, (Option<String>, Option<u32>)>,
    /// Invalidates polls of an item that is no longer shown.
    poll_ticket: u64,
    capability_known: bool,
    capability_probe_running: bool,
    /// AI 纪要 card folded; one choice for the whole session.
    minutes_collapsed: bool,
    /// The card shows the whole 纪要 without its height cap.
    minutes_full: bool,
    /// Manual minutes: the 纪要 button hid the finished card (kept for the session).
    minutes_hidden: bool,
    loading: bool,
    error: Option<String>,
    list: ListState,
    /// 并排 of a book: one list per column.
    list_o: ListState,
    list_t: ListState,
    /// Last seen tops of the two columns, and the matched row they were synced at.
    col_tops: (ListOffset, ListOffset),
    col_pair: Option<usize>,
    repair: Repair,
    repair_ticket: u64,
    repair_supported: bool,
    /// The agent offers owner edits of one book chapter (`reader_chapter_edit_v1`).
    chapter_edit_supported: bool,
    /// 翻译本章 of an English chapter without a translation.
    chapter_tr: chapter_translate::ChapterTr,
    /// Five-style marks on the AI chapter translation on screen.
    ai_marks: ai_marks::AiMarks,
    /// Pending 整理本章段落 request ids by item|revision|chapter|alignment version,
    /// written before each send so an unknown outcome is retried with the same id.
    repair_ids: std::collections::HashMap<String, String>,
    repair_store: Option<std::path::PathBuf>,
    /// After a repair committed a new revision: the entry to show once it is reloaded.
    after_repair: Option<usize>,
    /// A reload of the same book (e.g. after a title edit) returns to this entry.
    reopen_at: Option<(String, usize)>,
    /// The book's chapter list replaces the text while open.
    toc_open: bool,
    chapter_loading: bool,
    chapter_ticket: u64,
    /// The agent stayed busy (`library_loading`) through the bounded read
    /// retries: what 重试 reads again. Nothing loaded is dropped meanwhile.
    busy_retry: Option<BusyRetry>,
    reading_pct: u8,
    rail_bounds: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    rail_dragging: Rc<std::cell::Cell<bool>>,
    /// What the 纪要 card row showed when last measured; a change remeasures it.
    card_key: Option<(Option<MinutesState>, usize, bool, bool, MinutesAsk, bool)>,
    /// The pane is short (a small share of the conversation split): the title
    /// takes one line and its translation is left out, so text stays in view.
    compact: bool,
    /// Window bounds of the 纪要 button, last frame (tests and walks click it).
    minutes_button: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    /// 重点导航: the pointer is over the tick rail / its list, or a click pinned it open.
    nav_hover: bool,
    pub nav_pinned: bool,
    nav_index: f32,
    nav_anchor: f32,
    /// The agent serves exact historical revision text (`reader_revision_text_v1`).
    revision_text_supported: bool,
    /// 对比修改 shown in place of the text, and what 查看本次修改 would open.
    compare: Option<(Entity<crate::compare::RevisionCompare>, Subscription)>,
    compare_offer: Option<crate::compare::CompareSpec>,
    /// 格式整理 of the target on screen and 批量格式整理 of a folder.
    fmt: format_cleanup::Fmt,
    /// 古籍 display choices by item id (kept by the shell), display only.
    item_styles: HashMap<String, memex_client::ItemStyle>,
    /// 竖排: pixels scrolled leftwards from the right edge, the strip's last bounds,
    /// and its columns for the text and geometry they were laid out for.
    vscroll: f32,
    vbounds: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    vcols: Option<(VKey, Rc<Vec<crate::vertical::Column>>)>,
    /// 长文阅读 v2: 目录, 重点 switch, rail, dial, knobs, B2 state and marks.
    pub lr: longread::LongRead,
    /// 阅读记录: the end card, the ⏱ timer and the 阅读统计 panel.
    pub passes: passes_ui::Passes,
    /// The item's cover above the text (`item_cover_v1`), outside its coordinates.
    cover: cover::CoverSlot,
    /// The reading area takes focus on a click, so mark shortcuts reach it only there.
    focus: FocusHandle,
}

/// What 竖排 columns were laid out for: item, revision, chapter, side, script, cells per column.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VKey(String, String, Option<usize>, bool, memex_client::opencc::Script, usize, usize);

/// The list's first row is the AI 纪要 card (empty when there is none), so the
/// card scrolls away with the article instead of growing the fixed header.
/// Article rows start after it.
const LEAD: usize = 1;

impl EventEmitter<ReaderEvent> for Reader {}

/// Colors and reading size of one frame; bilingual originals are secondary.
#[derive(Clone)]
struct Palette {
    serif: SharedString,
    /// 简繁 of the shown text (display only; selection is off while converted).
    script: memex_client::opencc::Script,
    body_size: f32,
    /// Widest text line: 720 px, wider in the long-read list (`longread::MEASURE`).
    measure: f32,
    fg: Hsla,
    muted: Hsla,
    warm: Hsla,
    rose: Hsla,
    code_bg: Hsla,
    accent: Hsla,
    /// 重点标注: warm yellow ground, the one muted blue, rose-red.
    hl_bg: Hsla,
    pos: Hsla,
    neg: Hsla,
}

impl Palette {
    fn new(theme: &gpui_kit::component::Theme) -> Self {
        let dark = theme.background.l < 0.5;
        Palette {
            serif: SharedString::from(memex_client::reading_family()),
            script: Default::default(),
            body_size: 18.,
            measure: 720.,
            fg: theme.foreground,
            muted: theme.muted_foreground,
            // A muted warm (rose-leaning) tone, readable but clearly secondary.
            warm: if dark { hsla(350. / 360., 0.16, 0.74, 1.) } else { hsla(350. / 360., 0.14, 0.38, 1.) },
            rose: theme.primary,
            code_bg: theme.muted,
            accent: theme.primary,
            hl_bg: if dark { hsla(42. / 360., 0.55, 0.42, 0.42) } else { hsla(46. / 360., 0.95, 0.70, 0.55) },
            pos: if dark { hsla(212. / 360., 0.42, 0.72, 1.) } else { hsla(212. / 360., 0.40, 0.40, 1.) },
            neg: if dark { hsla(352. / 360., 0.68, 0.70, 1.) } else { hsla(352. / 360., 0.62, 0.45, 1.) },
        }
    }

    /// Verdict chip color: 有误 rose-red, 存疑 amber, 支持 sage, others neutral.
    fn verdict(&self, v: &Verdict, dark: bool) -> Hsla {
        match v {
            Verdict::Incorrect => self.neg,
            Verdict::Disputed => if dark { hsla(38. / 360., 0.70, 0.62, 1.) } else { hsla(34. / 360., 0.78, 0.38, 1.) },
            Verdict::Supported => if dark { hsla(140. / 360., 0.22, 0.64, 1.) } else { hsla(140. / 360., 0.24, 0.34, 1.) },
            _ => self.muted,
        }
    }
}

impl Reader {
    pub fn new(client: Client, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            client,
            reading_size: Default::default(),
            editing: Default::default(),
            edit_allowed: false,
            annotations_allowed: false,
            annotations: None,
            annotation_saving: false,
            annotation_notice: None,
            selected: Default::default(),
            images: Default::default(),
            model: ReaderModel::default(),
            translate: Translate::default(),
            insights: Insights::default(),
            ticket: 0,
            insight_ticket: 0,
            insight_polling: false,
            picked: None,
            skipped_highlights: 0,
            item_dates: HashMap::new(),
            poll_ticket: 0,
            capability_known: false,
            capability_probe_running: false,
            minutes_collapsed: false,
            minutes_full: false,
            minutes_hidden: false,
            nav_hover: false,
            nav_pinned: false,
            nav_index: 0.,
            nav_anchor: 0.5,
            loading: false,
            error: None,
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            list_o: ListState::new(0, ListAlignment::Top, px(800.)),
            list_t: ListState::new(0, ListAlignment::Top, px(800.)),
            col_tops: Default::default(),
            col_pair: None,
            repair: Repair::Idle,
            repair_ticket: 0,
            repair_supported: false,
            repair_ids: Default::default(),
            chapter_edit_supported: false,
            chapter_tr: Default::default(),
            ai_marks: Default::default(),
            repair_store: None,
            after_repair: None,
            reopen_at: None,
            toc_open: false,
            chapter_loading: false,
            chapter_ticket: 0,
            busy_retry: None,
            reading_pct: 0,
            rail_bounds: Default::default(),
            rail_dragging: Default::default(),
            card_key: None,
            compact: false,
            minutes_button: Default::default(),
            revision_text_supported: false,
            compare: None,
            compare_offer: None,
            fmt: Default::default(),
            item_styles: Default::default(),
            vscroll: 0.,
            vbounds: Default::default(),
            vcols: None,
            lr: Default::default(),
            passes: Default::default(),
            cover: Default::default(),
            focus: cx.focus_handle(),
        };
        let reader = cx.entity().downgrade();
        this.list.set_scroll_handler(move |_, _, cx| {
            // The list is mutably borrowed during this callback: only invalidate.
            reader.update(cx, |_, cx| cx.notify()).ok();
        });
        for list in [this.list_o.clone(), this.list_t.clone()] {
            let reader = cx.entity().downgrade();
            list.set_scroll_handler(move |_, _, cx| { reader.update(cx, |_, cx| cx.notify()).ok(); });
        }
        this.insights.emphasis = this.lr.filter.shows_ai();
        this.probe_capability(cx);
        this
    }

    /// Capability reads recover from temporary unavailability without requiring
    /// another item to be opened. Never retry generation from this probe.
    pub fn probe_capability(&mut self, cx: &mut Context<Self>) {
        if self.capability_probe_running { return; }
        self.capability_probe_running = true;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let mut result = client.capabilities();
            for delay in [500, 1000, 2000] {
                if !matches!(&result, Err(memex_client::ClientError::Unavailable | memex_client::ClientError::Timeout)) { break; }
                std::thread::sleep(Duration::from_millis(delay));
                result = client.capabilities();
            }
            result
        });
        cx.spawn(async move |this, cx| {
            let caps = task.await;
            this.update(cx, |this, cx| {
                this.capability_probe_running = false;
                if let Ok(caps) = caps {
                    this.capability_known = true;
                    this.edit_allowed = caps.features.iter().any(|f| f == "markdown_edit_v1");
                    this.annotations_allowed = caps.features.iter().any(|f| f == "reading_annotations_v1");
                    this.repair_supported = caps.features.iter().any(|f| f == memex_client::book::REPAIR_FEATURE);
                    this.chapter_edit_supported = caps.features.iter().any(|f| f == memex_client::book::EDIT_FEATURE);
                    this.chapter_tr.supported = caps.features.iter().any(|f| f == memex_client::book::TRANSLATION_FEATURE);
                    let (metadata, alignment) = (caps.features.iter().any(|f| f == memex_client::book::TRANSLATION_METADATA_FEATURE), caps.features.iter().any(|f| f == memex_client::book::TRANSLATION_ALIGNMENT_FEATURE));
                    if (metadata, alignment) != (this.chapter_tr.metadata, this.chapter_tr.alignment) {
                        // A different way to read: look the chapter up again.
                        this.chapter_tr.metadata = metadata;
                        this.chapter_tr.alignment = alignment;
                        this.chapter_tr.key = None;
                    }
                    let ai_marks = caps.features.iter().any(|f| f == memex_client::ai_marks::AI_ANNOTATIONS_FEATURE);
                    if ai_marks != this.ai_marks.supported {
                        this.ai_marks.supported = ai_marks;
                        this.ai_marks.loaded_for = None;
                        this.ai_load(cx);
                    }
                    this.revision_text_supported = caps.features.iter().any(|f| f == memex_client::book::REVISION_TEXT_FEATURE);
                    this.fmt.supported = caps.features.iter().any(|f| f == memex_client::format::FORMAT_FEATURE);
                    this.fmt.batch_supported = this.fmt.supported && caps.features.iter().any(|f| f == memex_client::format::FORMAT_BATCH_FEATURE);
                    this.refresh_chapter_translation(cx);
                    this.translate.set_capability(caps.translation);
                    this.translate.set_minutes_capability(caps.minutes);
                    this.translate.set_manual_minutes(caps.minutes_request);
                    this.insights.set_capability(caps.insights);
                    if this.annotations.is_none() { this.load_annotations(cx); }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Restores the remembered mode without reporting it as a change.
    pub fn restore_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.model.set_mode(mode);
        self.pass_mode(mode);
        self.list.reset(self.model.layout().rows.len() + LEAD);
        cx.notify();
    }

    /// The list row's 发布日期 / 时长 for `item_id` (either may be unknown).
    pub fn set_item_dates(&mut self, item_id: &str, published_at: Option<String>, duration_seconds: Option<u32>, cx: &mut Context<Self>) {
        let dates = (published_at, duration_seconds);
        if self.item_dates.get(item_id) != Some(&dates) {
            self.item_dates.insert(item_id.to_string(), dates);
            cx.notify();
        }
    }

    /// `发布 2026-10-30 · 1:00:26` for `item_id`, when either part is known.
    pub fn dates_line(&self, item_id: &str) -> Option<String> {
        self.item_dates.get(item_id).and_then(|(p, d)| memex_client::published_line(p.as_deref(), *d))
    }

    pub fn open(&mut self, item_id: &str, title: &str, new_tab: bool, cx: &mut Context<Self>) {
        if !self.capability_known { self.probe_capability(cx); }
        if self.model.active_item() == Some(item_id) && self.model.detail().is_some() {
            if new_tab { if let Some(i) = self.model.active() { self.pin_tab(i, cx); } }
            return;
        }
        if self.blocks_navigation(cx) { return; }
        self.editing = Default::default();
        self.reopen_at = None;
        self.model.open(item_id, title, new_tab);
        self.fetch(cx);
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.blocks_navigation(cx) { return; }
        // A refresh looks the AI translation up again (a miss is never remembered).
        self.chapter_tr.key = None;
        self.reopen_at = self.model.book().map(|b| (b.doc.item_id.clone(), b.at));
        if let Some(i) = self.model.active() {
            self.model.activate(i);
            self.fetch(cx);
        }
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        self.ticket += 1;
        self.selected = Default::default();
        self.annotations = None;
        self.annotation_notice = None;
        self.rail_dragging.set(false);
        self.images.clear();
        self.poll_ticket += 1;
        self.insight_ticket += 1;
        self.insight_polling = false;
        self.picked = None;
        self.lr_reset();
        self.ai_reset();
        self.pass_reset(cx);
        let ticket = self.ticket;
        self.error = None;
        self.reading_pct = 0;
        self.list.reset(0);
        self.reset_columns();
        if self.after_repair.is_none() { self.repair = Repair::Idle; }
        self.repair_ticket += 1;
        self.toc_open = false;
        self.chapter_loading = false;
        self.chapter_ticket += 1;
        self.busy_retry = None;
        // A comparison belongs to its item; another item closes it and its offer.
        let active = self.model.active_item().map(str::to_string);
        // (A batch panel compares any of its rows, not only the open item.)
        if self.fmt.batch.is_none() && self.compare.as_ref().is_some_and(|(c, _)| Some(&c.read(cx).spec().item_id) != active.as_ref()) { self.compare = None; }
        if self.fmt.batch.is_none() && self.fmt.review.as_ref().is_some_and(|r| Some(&r.handle.item_id) != active.as_ref()) { self.fmt.review = None; }
        if self.compare_offer.as_ref().is_some_and(|o| Some(&o.item_id) != active.as_ref()) { self.compare_offer = None; }
        if !self.capability_known {
            self.probe_capability(cx);
        }
        self.cover_reset(self.model.active_item().map(str::to_string).as_deref());
        let Some(item_id) = self.model.active_item().map(str::to_string) else {
            self.loading = false;
            cx.notify();
            return;
        };
        self.loading = true;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            // A trashed item reads as not found: say it is in the trash instead.
            let detail = client.item_detail(&item_id).map_err(|e| {
                let not_found = matches!(&e, memex_client::ClientError::Daemon(c) if c == "item_not_found" || c == "library_resource_not_found");
                let trashed = not_found && client.capabilities().is_ok_and(|c| c.trash) && client.trash_all().is_ok_and(|t| t.iter().any(|x| x.item_id == item_id));
                if trashed { memex_client::ClientError::Daemon("item_in_trash".into()) } else { e }
            })?;
            // A paired book shows its own editions, read before anything is drawn so
            // its Chinese canonical text never appears as the original.
            let book = open_book(&client, &detail.canonical.item_id, &detail.canonical.revision_id);
            Ok::<_, memex_client::ClientError>((detail, book))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((detail, book)) => {
                        if !this.blocks_navigation(cx) && this.editing.revision != detail.canonical.revision_id {
                            this.editing = Default::default();
                        }
                        let (id, state, reason) = (detail.canonical.item_id.clone(), detail.analysis.translation_state, detail.analysis.translation_reason.clone());
                        let minutes = detail.analysis.ai_minutes.clone();
                        let insights = detail.analysis.ai_insights.clone();
                        let progress = detail.analysis.translation_progress;
                        let diagnostics = (detail.analysis.translation_failure_detail.clone(), detail.analysis.untranslated_blocks);
                        let mut chinese = looks_chinese(&detail.canonical.title, &detail.canonical.markdown);
                        this.model.load(detail);
                        match book {
                            Ok(Some((book, original, translation, alignment))) => {
                                // Both editions exist: nothing to translate.
                                chinese = true;
                                this.model.set_book(book);
                                this.model.set_chapter(0, original, translation, alignment);
                                this.reset_columns();
                                let reopen = this.reopen_at.take().filter(|(id, at)| *id == this.model.book().map(|b| b.doc.item_id.clone()).unwrap_or_default() && *at > 0);
                                if let Some(at) = this.after_repair.take() { this.go_chapter_keeping(at, true, cx); }
                                else if let Some((_, at)) = reopen { this.go_chapter_keeping(at, false, cx); }
                            }
                            Ok(None) => {}
                            Err(e) if e.is_busy() => this.busy_retry = Some(BusyRetry::Item),
                            Err(e) => this.model.set_notice(Some(txf("未能读取书籍章节（{0}），暂按普通文档显示", &[&e.message()]).to_string())),
                        }
                        this.refresh_chapter_translation(cx);
                        this.load_annotations(cx);
                        this.lr_load(cx);
                        this.list.reset(this.model.layout().rows.len() + LEAD);
                        this.cover_load(id.clone(), cx);
                        let next = this.translate.reset(&id, state, reason, minutes, chinese);
                        this.translate.set_progress(progress);
                        this.translate.set_diagnostics(diagnostics.0, diagnostics.1);
                        this.follow(next, cx);
                        let next = this.insights.reset(&id, insights);
                        this.follow_insights(next, cx);
                    }
                    Err(e) => {
                        if e.is_busy() { this.busy_retry = Some(BusyRetry::Item); }
                        this.error = Some(e.message());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows entry `at` of the open book (both sides of that chapter).
    pub fn go_chapter(&mut self, at: usize, cx: &mut Context<Self>) {
        // An unsaved or unconfirmed chapter draft keeps its chapter.
        if self.editing.chapter.is_some() {
            if self.blocks_navigation(cx) { return; }
            self.editing = Default::default();
        }
        self.go_chapter_keeping(at, false, cx);
    }

    fn go_chapter_keeping(&mut self, at: usize, keep_repair: bool, cx: &mut Context<Self>) {
        let Some(book) = self.model.book().cloned() else { return };
        if at >= book.entries.len() { return; }
        if !keep_repair {
            self.repair = Repair::Idle;
            self.repair_ticket += 1;
        }
        self.toc_open = false;
        self.chapter_ticket += 1;
        let ticket = self.chapter_ticket;
        self.chapter_loading = true;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { load_entry(&client, &book, at) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.chapter_ticket != ticket { return; }
                this.chapter_loading = false;
                this.busy_retry = None;
                match result {
                    Ok((original, translation, alignment)) => {
                        // A busy index read keeps the chapter; 重试 reads it again.
                        if alignment == crate::model::Paired::Busy { this.busy_retry = Some(BusyRetry::Chapter(at)); }
                        this.model.set_chapter(at, original, translation, alignment);
                        this.model.set_notice(None);
                        this.list.reset(this.model.layout().rows.len() + LEAD);
                        this.reset_columns();
                        this.refresh_chapter_translation(cx);
                    }
                    Err(e) if e.is_busy() => this.busy_retry = Some(BusyRetry::Chapter(at)),
                    Err(e) => this.model.set_notice(Some(txf("这一章没有读取成功：{0}", &[&e.message()]).to_string())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn reset_columns(&mut self) {
        self.list_o.reset(self.model.original().len());
        self.list_t.reset(self.model.translation().len());
        self.col_tops = Default::default();
        self.col_pair = None;
    }

    /// Restores the remembered 段落 / 并排 choice without reporting it.
    pub fn restore_book_parallel(&mut self, parallel: bool, cx: &mut Context<Self>) {
        self.model.set_parallel(parallel);
        cx.notify();
    }

    pub fn set_book_parallel(&mut self, parallel: bool, cx: &mut Context<Self>) {
        self.model.set_parallel(parallel);
        self.list.reset(self.model.layout().rows.len() + LEAD);
        self.reset_columns();
        cx.emit(ReaderEvent::BookParallelChanged(parallel));
        cx.notify();
    }

    /// Where pending repair request ids are kept (next to the desktop settings).
    pub fn set_repair_store(&mut self, path: Option<std::path::PathBuf>) {
        self.repair_ids = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        self.repair_store = path;
    }

    fn save_repair_ids(&self) {
        if let Some(path) = &self.repair_store {
            if let Ok(bytes) = serde_json::to_vec(&self.repair_ids) {
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, bytes).is_ok() { let _ = std::fs::rename(&tmp, path); }
            }
        }
    }

    /// 并排: the column the user scrolled leads; the other one goes to the start
    /// of the last matched row at or above the leader's top. One-sided and
    /// unresolved rows never move the other column; nothing is interpolated.
    fn sync_columns(&mut self) {
        let rows = self.model.aligned().to_vec();
        self.sync_columns_by(&rows);
    }

    /// The same sync over any recorded rows (an AI translation's blocks too).
    fn sync_columns_by(&mut self, rows: &[crate::model::AlignedRow]) {
        let (o, t) = (self.list_o.logical_scroll_top(), self.list_t.logical_scroll_top());
        let (o_moved, t_moved) = (o != self.col_tops.0, t != self.col_tops.1);
        self.col_tops = (o, t);
        if o_moved == t_moved { return; }
        use memex_client::book::Relation;
        let lead = |r: &crate::model::AlignedRow| if o_moved { r.o.clone() } else { r.t.clone() };
        let top = if o_moved { o.item_ix } else { t.item_ix };
        let Some(p) = rows.iter().rposition(|r| r.relation == Relation::Matched && lead(r).start <= top) else { return };
        if self.col_pair == Some(p) { return; }
        self.col_pair = Some(p);
        let (follower, target) = if o_moved { (&self.list_t, rows[p].t.start) } else { (&self.list_o, rows[p].o.start) };
        follower.scroll_to(ListOffset { item_ix: target, offset_in_item: px(0.) });
        self.col_tops = (self.list_o.logical_scroll_top(), self.list_t.logical_scroll_top());
    }

    /// Scrolls one 并排 column to a block (walks and tests drive the sync with it).
    /// Walks: scrolls a 并排 / AI column to `y` px inside block `block`.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_scroll_column_by(&mut self, translation: bool, block: usize, y: f32, cx: &mut Context<Self>) {
        let list = if translation { &self.list_t } else { &self.list_o };
        list.scroll_to(ListOffset { item_ix: block, offset_in_item: px(y) });
        cx.notify();
    }
    pub fn scroll_column(&mut self, translation: bool, block: usize, cx: &mut Context<Self>) {
        let list = if translation { &self.list_t } else { &self.list_o };
        list.scroll_to(ListOffset { item_ix: block, offset_in_item: px(0.) });
        cx.notify();
    }

    /// The tops (block indexes) of the two 并排 columns.
    pub fn column_tops(&self) -> (usize, usize) {
        (self.list_o.logical_scroll_top().item_ix, self.list_t.logical_scroll_top().item_ix)
    }

    pub fn ask_repair(&mut self, cx: &mut Context<Self>) {
        self.repair = Repair::Confirm;
        cx.notify();
    }

    pub fn cancel_repair(&mut self, cx: &mut Context<Self>) {
        self.repair = Repair::Idle;
        cx.notify();
    }

    /// 整理本章段落: admits one repair of this chapter, then follows the job.
    pub fn start_repair(&mut self, cx: &mut Context<Self>) {
        let Some(book) = self.model.book().cloned() else { return };
        let Some(chapter) = book.paired_key(book.at) else { return };
        let version = match &book.alignment {
            crate::model::Paired::Index(a) => a.version.clone(),
            _ => return,
        };
        let key = format!("{}|{}|{}|{}", book.doc.item_id, book.doc.revision_id, chapter, version);
        let request_id = self.repair_ids.get(&key).cloned().unwrap_or_else(|| {
            use std::hash::{BuildHasher, Hasher};
            let n = |i: u64| { let mut h = std::collections::hash_map::RandomState::new().build_hasher(); h.write_u64(i); h.finish() };
            format!("mac-repair-{:016x}{:016x}", n(1), n(2))
        });
        // Written before sending: an unknown outcome is retried with this id.
        self.repair_ids.insert(key.clone(), request_id.clone());
        self.save_repair_ids();
        self.repair_ticket += 1;
        let ticket = self.repair_ticket;
        self.repair = Repair::Running { job_id: String::new(), stage: "queued".into() };
        cx.notify();
        let client = self.client.clone();
        let at = book.at;
        cx.spawn(async move |this, cx| {
            let doc = book.doc.clone();
            let admitted = cx.background_executor().spawn({ let client = client.clone(); let request_id = request_id.clone(); async move { client.reader_align_request(&doc, &chapter, &request_id, &version) } }).await;
            let mut job = match admitted {
                Ok(job) => job,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.repair_ticket != ticket { return; }
                        // library_loading is not a definite rejection in reader_v1: keep the id.
                        let unknown = matches!(e, memex_client::ClientError::Timeout | memex_client::ClientError::Unavailable | memex_client::ClientError::InvalidFrame) || e.is_busy();
                        if unknown {
                            this.repair = Repair::Unknown(txf("整理结果未知：{0}", &[&e.message()]));
                        } else {
                            this.repair_ids.remove(&key);
                            this.save_repair_ids();
                            this.repair = Repair::Failed(e.message());
                        }
                        cx.notify();
                    }).ok();
                    return;
                }
            };
            loop {
                let state = job.state;
                let alive = this.update(cx, |this, cx| {
                    if this.repair_ticket != ticket { return false; }
                    this.repair = Repair::Running { job_id: job.job_id.clone(), stage: job.stage.clone() };
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !alive { return; }
                if state.is_terminal() { break; }
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let (item, id) = (job.item_id.clone(), job.job_id.clone());
                match cx.background_executor().spawn({ let client = client.clone(); async move { client.reader_align_status(&item, &id) } }).await {
                    Ok(next) => job = next,
                    Err(e) => {
                        this.update(cx, |this, cx| {
                            if this.repair_ticket == ticket { this.repair = Repair::Unknown(txf("整理结果未知：{0}", &[&e.message()])); cx.notify(); }
                        }).ok();
                        return;
                    }
                }
            }
            this.update(cx, |this, cx| {
                if this.repair_ticket != ticket { return; }
                use memex_client::book::JobState;
                // Outcome unknown keeps the id: 查询原任务 replays it and gets the original job back.
                if job.state == JobState::OutcomeUnknown {
                    this.repair = Repair::Unknown(tx("结果未知：不确定模型请求是否完成，本章没有改动。可以查询原任务，不会自动重试。").into());
                    cx.notify();
                    return;
                }
                this.repair_ids.remove(&key);
                this.save_repair_ids();
                match job.state {
                    JobState::Succeeded => {
                        this.repair = Repair::Done;
                        let current = this.model.book().map(|b| b.doc.revision_id.clone());
                        if job.result_revision_id.is_some() && job.result_revision_id != current {
                            // A new revision: everything read before is stale.
                            this.after_repair = Some(at);
                            this.reload(cx);
                            // The item moved on: details refresh unless they hold a draft (then saving meets an honest conflict).
                            cx.emit(ReaderEvent::Saved);
                        } else {
                            this.go_chapter_keeping(at, true, cx);
                        }
                    }
                    JobState::Failed => {
                        let code = job.error_code.clone().unwrap_or_else(|| "reader_repair_failed".into());
                        this.repair = Repair::Failed(memex_client::ClientError::Daemon(code).message());
                    }
                    _ => {}
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// The paragraph-pairing line of a book chapter in 对照: what is known, the
    /// 整理本章段落 action and its progress.
    fn pairing_line(&self, cx: &mut Context<Self>) -> Option<Div> {
        let book = self.model.book()?;
        if self.model.mode() != Mode::Bilingual || book.paired_key(book.at).is_none() || self.chapter_loading { return None; }
        let theme = cx.theme().clone();
        use crate::model::Paired;
        use memex_client::book::AlignmentStatus;
        let index = match &book.alignment { Paired::Index(a) => Some(a), _ => None };
        let offer = self.repair_supported && index.is_some();
        let line = h_flex().gap_2().items_center().flex_wrap().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs();
        let button = |id: &'static str, label: &'static str, cx: &mut Context<Self>, f: fn(&mut Self, &mut Context<Self>)| {
            Button::new(id).xsmall().outline().label(tx(label)).on_click(cx.listener(move |r, _, _, cx| f(r, cx)))
        };
        let text = |t: String| div().child(t);
        Some(match &self.repair {
            Repair::Confirm => line.child(text(tx("只整理这一章：用 Mac 上的 Low dial 模型修正断段并匹配段落，最多两次请求。").into()))
                .child(button("repair-start", "开始整理", cx, Self::start_repair)).child(button("repair-cancel", "取消", cx, Self::cancel_repair)),
            Repair::Running { stage, .. } => line.child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(text(tx(match stage.as_str() {
                "formatting" => "正在整理格式",
                "matching" => "正在匹配段落",
                "committing" | "done" => "正在保存",
                _ => "排队中",
            }).into())),
            Repair::Unknown(m) => line.child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning)).child(text(m.clone())).child(button("repair-retry", "查询原任务", cx, Self::start_repair)),
            Repair::Failed(m) => line.child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning)).child(text(m.clone())).when(offer, |d| d.child(button("repair-again", "再试一次", cx, Self::ask_repair))),
            Repair::Done => line.child(Icon::new(IconName::Check).xsmall().text_color(theme.primary)).child(text(tx("本章已重新整理").into())),
            Repair::Idle => {
                let (message, ask) = match (&book.alignment, index.map(|a| a.status)) {
                    (Paired::Invalid(_), _) => (tx("段落对照与本章正文不一致，暂不使用"), false),
                    // Shown by the busy line below the chapter bar.
                    (Paired::Busy, _) => return None,
                    (_, Some(AlignmentStatus::Aligned)) => (tx("段落对应由 AI 整理，未经核对"), true),
                    (_, Some(AlignmentStatus::Partial)) => (tx("这一章部分段落未匹配"), true),
                    _ => (tx("这一章还没有段落对照"), true),
                };
                line.text_color(theme.muted_foreground).child(text(message.into())).when(offer && ask, |d| d.child(button("repair-ask", "整理本章段落", cx, Self::ask_repair)))
            }
        })
    }

    /// 重试 after 「Mac 正忙，稍后重试」: reads the same item or chapter again.
    /// Drafts, the comparison and remembered AI versions are untouched.
    pub fn retry_busy(&mut self, cx: &mut Context<Self>) {
        match self.busy_retry.take() {
            Some(BusyRetry::Item) => self.fetch(cx),
            Some(BusyRetry::Chapter(at)) => self.go_chapter_keeping(at, true, cx),
            None => {}
        }
        cx.notify();
    }

    /// The calm busy line: no warning colour, the data is not in doubt.
    fn busy_line(&self, cx: &mut Context<Self>) -> Option<Div> {
        self.busy_retry?;
        let theme = cx.theme();
        Some(h_flex().gap_2().items_center().p_2().rounded(px(6.)).bg(theme.secondary).text_xs()
            .child(Icon::new(IconName::Info).xsmall().text_color(theme.muted_foreground))
            .child(div().flex_1().child(tx("Mac 正忙，稍后重试")))
            .child(Button::new("reader-busy-retry").xsmall().outline().label(tx("重试")).on_click(cx.listener(|r, _, _, cx| r.retry_busy(cx)))))
    }

    /// Walks: what a busy line would read again, and the book chapter shown.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_busy(&self) -> String {
        format!("busy={:?} book_at={:?} error={:?}", self.busy_retry, self.model.book().map(|b| b.at), self.error)
    }

    pub fn toggle_toc(&mut self, cx: &mut Context<Self>) {
        self.toc_open = !self.toc_open;
        cx.notify();
    }

    /// Previous / next chapter, chapter list and the chapter-pairing note.
    fn book_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let book = self.model.book()?;
        let theme = cx.theme();
        let (at, n) = (book.at, book.entries.len());
        Some(
            h_flex()
                .gap_2()
                .items_center()
                .child(Button::new("book-prev").small().ghost().icon(IconName::ChevronLeft).disabled(at == 0 || self.chapter_loading)
                    .tooltip(tx("上一章")).on_click(cx.listener(move |r, _, _, cx| r.go_chapter(at.saturating_sub(1), cx))))
                .child(Button::new("book-toc").small().outline().when(self.toc_open, |b| b.primary()).icon(IconName::Menu)
                    .label(txf("{0} / {1} · {2}", &[&(at + 1), &n, &book.title(at)]).to_string())
                    .on_click(cx.listener(|r, _, _, cx| r.toggle_toc(cx))))
                .child(Button::new("book-next").small().ghost().icon(IconName::ChevronRight).disabled(at + 1 >= n || self.chapter_loading)
                    .tooltip(tx("下一章")).on_click(cx.listener(move |r, _, _, cx| r.go_chapter(at + 1, cx))))
                // Always in 对照. 段落 needs a counterpart chapter or an AI translation with
                // recorded rows; otherwise (front matter, a legacy AI translation) it is shown
                // disabled and the chapter stays 并排, as on the phone.
                .when(self.model.mode() == Mode::Bilingual, |d| {
                    let pairs = book.paired_key(at).is_some() || self.ai_rows().is_some();
                    let parallel = self.model.parallel() || !pairs;
                    d.child(h_flex().rounded(px(6.)).bg(theme.muted).p(px(2.))
                        .child(Button::new("book-layout-pairs").xsmall().label(tx("段落")).when(!parallel, |b| b.primary()).when(parallel, |b| b.ghost())
                            .disabled(!pairs).when(!pairs, |b| b.tooltip(tx("这一章没有逐段对应，只能并排")))
                            .on_click(cx.listener(|r, _, _, cx| r.set_book_parallel(false, cx))))
                        .child(Button::new("book-layout-parallel").xsmall().label(tx("并排")).when(parallel, |b| b.primary()).when(!parallel, |b| b.ghost())
                            .on_click(cx.listener(|r, _, _, cx| r.set_book_parallel(true, cx)))))
                })
                .when(self.chapter_loading, |d| d.child(div().text_xs().text_color(theme.muted_foreground).child(tx("正在载入这一章…")))),
        )
    }

    /// 对照 of a book: the two editions of the chapter side by side, each scrolling
    /// on its own (chapters correspond; paragraphs are not aligned yet).
    fn book_columns(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(book) = self.model.book() else { return div().into_any_element() };
        let theme = cx.theme().clone();
        let reader = cx.entity().downgrade();
        let language = |side: &str| match book.language(side).as_str() {
            "en" => "English".to_string(),
            "zh" => "中文".to_string(),
            other => other.to_string(),
        };
        let end = self.chapter_after();
        let lead_original = !self.model.original().is_empty();
        let column = |id: &'static str, label: String, state: ListState, blocks: Vec<Block>, translation: bool, missing: &'static str| {
            let p = palette.clone();
            let reader = reader.clone();
            let end = end.clone().filter(|_| translation != lead_original);
            let n = blocks.len();
            let body = if blocks.is_empty() {
                v_flex().flex_1().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(tx(missing)).into_any_element()
            } else {
                list(state, move |ix, _window, cx| match blocks.get(ix) {
                    Some(b) => {
                        let el = block_element(b, &p, false, false, &[], format!("{}{ix}", if translation { 't' } else { 'o' }), &reader, cx);
                        match &end {
                            Some(next) if ix + 1 == n => v_flex().child(el).child(chapter_end(next.clone(), &p, &reader, "chapter-end-column")).into_any_element(),
                            _ => el.into_any_element(),
                        }
                    }
                    None => div().into_any_element(),
                })
                .flex_1()
                .into_any_element()
            };
            v_flex()
                .id(id)
                .flex_1()
                .h_full()
                .min_w_0()
                .min_h_0()
                .gap_1()
                .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.primary).child(label))
                .child(body)
        };
        h_flex()
            .flex_1()
            .min_h_0()
            .items_stretch()
            .gap_6()
            .child(column("book-original", format!("{} · {}", tx("原文"), language("original")), self.list_o.clone(), self.model.original().to_vec(), false, "这一章只有译文，原文版本里没有对应章节"))
            .child(div().flex_none().w(px(1.)).h_full().bg(theme.border))
            .child(column("book-translation", format!("{} · {}", tx("译文"), language("translation")), self.list_t.clone(), self.model.translation().to_vec(), true, "这一章只有原文，译本里没有对应章节"))
            .into_any_element()
    }

    /// The book's chapters in reading order; the current one is marked.
    fn toc_panel(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(book) = self.model.book() else { return div().into_any_element() };
        let theme = cx.theme().clone();
        let top = (0..book.entries.len()).map(|i| book.level(i)).min().unwrap_or(1);
        let mut col = v_flex().gap(px(2.)).pb_6();
        for i in 0..book.entries.len() {
            let current = i == book.at;
            let e = book.entries[i];
            let only = match (e.original, e.translation) {
                (Some(_), None) => Some(tx("仅原文")),
                (None, Some(_)) => Some(tx("仅译文")),
                _ => None,
            };
            col = col.child(
                h_flex()
                    .id(("book-chapter", i))
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .pl(px(12. + 16. * f32::from(book.level(i).saturating_sub(top))))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .font_family(palette.serif.clone())
                    .when(current, |d| d.bg(theme.primary.opacity(0.12)).text_color(theme.primary))
                    .when(!current, |d| d.hover(|d| d.bg(theme.secondary)))
                    .child(div().flex_1().min_w_0().truncate().child(book.title(i)))
                    .when_some(only, |d, o| d.child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(o)))
                    .on_click(cx.listener(move |r, _, _, cx| r.go_chapter(i, cx))),
            );
        }
        div().id("book-toc-list").flex_1().min_h_0().overflow_y_scroll().max_w(px(720.)).child(col).into_any_element()
    }

    /// 翻译此文: one `translation_request`, then follow the job until it ends.
    pub fn request_translation(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.model.active_item().map(str::to_string) else { return };
        if !self.translate.begin() {
            return;
        }
        self.poll_ticket += 1;
        let ticket = self.poll_ticket;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.translation_request(&item_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.poll_ticket != ticket {
                    return;
                }
                let started = result.is_ok();
                let next = this.translate.on_request(result);
                if started {
                    // The side panel shows the new states (e.g. 纪要生成中…) too.
                    cx.emit(ReaderEvent::TranslationReady);
                }
                this.follow(next, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The 纪要 button: ask for the minutes when there are none or they failed;
    /// once they exist, show or hide the card (kept for the session).
    pub fn press_minutes(&mut self, cx: &mut Context<Self>) {
        match self.translate.minutes_button() {
            Some(ButtonState::Done) => {
                self.minutes_hidden = !self.minutes_hidden;
                cx.notify();
            }
            Some(ButtonState::NotGenerated | ButtonState::Failed(_)) => self.request_minutes(cx),
            _ => {}
        }
    }

    /// `minutes_request` for the open item (manual minutes only), then follows
    /// the minutes with the same backoff as a translation until they end.
    pub fn request_minutes(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.model.active_item().map(str::to_string) else { return };
        if !self.translate.begin_minutes() {
            return;
        }
        self.minutes_hidden = false;
        // One poll loop follows both the translation and the minutes.
        self.poll_ticket += 1;
        let ticket = self.poll_ticket;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.minutes_request(&item_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.poll_ticket != ticket {
                    return;
                }
                let started = result.is_ok();
                let next = this.translate.on_minutes_request(result);
                // The translation may still be running: keep following it too.
                let next = match (next, this.translate.phase()) {
                    (Next::Stop, crate::translate::Phase::Waiting { attempt, .. }) => Next::Poll(crate::translate::backoff(*attempt)),
                    (next, _) => next,
                };
                if started {
                    // The side panel shows 纪要生成中… too.
                    cx.emit(ReaderEvent::TranslationReady);
                }
                this.follow(next, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn follow(&mut self, next: Next, cx: &mut Context<Self>) {
        match next {
            Next::Stop => {}
            Next::Reload => {
                cx.emit(ReaderEvent::TranslationReady);
                self.reload(cx);
            }
            Next::Poll(delay) => {
                let Some(item_id) = self.model.active_item().map(str::to_string) else { return };
                let ticket = self.poll_ticket;
                let client = self.client.clone();
                let executor = cx.background_executor().clone();
                let task = cx.background_executor().spawn(async move {
                    executor.timer(delay).await;
                    client.library_item_read(&item_id)
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    this.update(cx, |this, cx| {
                        if this.poll_ticket != ticket {
                            return;
                        }
                        let next = this.translate.on_poll(result);
                        this.follow(next, cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// 重点标注 / 金句 / 事实核查 pressed: switch the view or the emphasis, and
    /// ask the agent for the kind when it has not been generated.
    pub fn press_insight(&mut self, kind: InsightKind, cx: &mut Context<Self>) {
        if let Some(kind) = self.insights.press(kind) {
            self.send_insight(kind, cx);
        }
        // 重点标注 on / off is the AI layer of the 重点 switch.
        if kind == InsightKind::Highlights {
            use crate::nav::Filter;
            self.lr.filter = match (self.insights.emphasis, self.lr.filter) {
                (true, Filter::Mine | Filter::Off) => Filter::All,
                (false, Filter::All) => Filter::Mine,
                (false, Filter::Ai) => Filter::Off,
                (_, f) => f,
            };
        }
        self.list.remeasure();
        cx.notify();
    }

    /// 重试 / 生成 in a view whose kind failed or was not generated.
    pub fn retry_insight(&mut self, kind: InsightKind, cx: &mut Context<Self>) {
        if self.insights.begin(kind) {
            self.send_insight(kind, cx);
        }
        cx.notify();
    }

    /// Shows the text (or a 金句 / 事实核查 view) without sending anything.
    pub fn set_insight_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.insights.view = view;
        cx.notify();
    }

    /// Turns 重点标注 on or off without sending anything.
    pub fn set_emphasis(&mut self, on: bool, cx: &mut Context<Self>) {
        self.insights.emphasis = on;
        self.lr.filter = if on { crate::nav::Filter::All } else { crate::nav::Filter::Mine };
        self.list.remeasure();
        cx.notify();
    }

    /// Highlight quotes that could not be placed in the last frame (0 when correct).
    pub fn skipped_highlights(&self) -> usize {
        self.skipped_highlights
    }

    fn send_insight(&mut self, kind: InsightKind, cx: &mut Context<Self>) {
        let Some(item_id) = self.model.active_item().map(str::to_string) else { return };
        let ticket = self.insight_ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.insight_request(&item_id, &[kind]) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.insight_ticket != ticket {
                    return;
                }
                let next = this.insights.on_request(kind, result);
                this.list.remeasure();
                this.follow_insights(next, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the item again with the translate backoff while a kind is generating.
    fn follow_insights(&mut self, next: Next, cx: &mut Context<Self>) {
        let Next::Poll(delay) = next else { return };
        if self.insight_polling {
            return;
        }
        let Some(item_id) = self.model.active_item().map(str::to_string) else { return };
        self.insight_polling = true;
        let ticket = self.insight_ticket;
        let client = self.client.clone();
        let executor = cx.background_executor().clone();
        let task = cx.background_executor().spawn(async move {
            executor.timer(delay).await;
            client.library_item_read(&item_id)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.insight_ticket != ticket {
                    return;
                }
                this.insight_polling = false;
                let next = this.insights.on_poll(result);
                this.list.remeasure();
                this.follow_insights(next, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 定位原文: back to the reading view, scrolled to the block, which flashes.
    pub fn locate(&mut self, block: usize, cx: &mut Context<Self>) {
        self.insights.view = View::Text;
        if self.model.layout().shown == Mode::Translation {
            self.model.set_mode(Mode::Original);
        }
        self.list.reset(self.model.layout().rows.len() + LEAD);
        self.flash(block, block, cx);
    }

    /// 重点导航: scrolls to layout row `row` on the side being read (the mode
    /// stays) and flashes its block.
    pub fn jump_to_mark(&mut self, row: usize, block: usize, cx: &mut Context<Self>) {
        self.nav_pinned = false;
        self.nav_hover = false;
        self.flash(row, block, cx);
    }

    fn flash(&mut self, row: usize, block: usize, cx: &mut Context<Self>) {
        self.model.set_highlight(Some(block));
        self.list.scroll_to(ListOffset { item_ix: row + LEAD, offset_in_item: px(0.) });
        let ticket = self.ticket;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(2400)).await;
            this.update(cx, |this, cx| {
                if this.ticket == ticket && this.model.highlight() == Some(block) {
                    this.model.set_highlight(None);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 双击标签：固定它，单击打开的条目不再替换它。
    pub fn pin_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.model.pin(index);
        cx.notify();
    }

    /// Lays rows out again (a typeface changed outside the reader).
    pub fn remeasure(&mut self, cx: &mut Context<Self>) {
        self.list.remeasure();
        self.list_o.remeasure();
        self.list_t.remeasure();
        cx.notify();
    }

    /// The book's chapter after the one on screen: `Some(None)` at the last chapter.
    pub fn chapter_after(&self) -> Option<Option<(usize, String)>> {
        let book = self.model.book()?;
        Some((book.at + 1 < book.entries.len()).then(|| (book.at + 1, book.title(book.at + 1))))
    }

    fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.blocks_navigation(cx) { return; }
        self.editing = Default::default();
        let id = self.model.activate(index);
        self.fetch(cx);
        cx.emit(ReaderEvent::Activated(id));
    }

    fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.blocks_navigation(cx) { return; }
        self.editing = Default::default();
        let id = self.model.close(index);
        self.fetch(cx);
        cx.emit(ReaderEvent::Activated(id));
    }

    /// Folds or unfolds the AI 纪要 card (kept for the session).
    pub fn set_minutes_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.minutes_collapsed = collapsed;
        cx.notify();
    }

    /// 展开全部 / 收回: the card at full height or with its scroll cap (kept for the session).
    pub fn set_minutes_full(&mut self, full: bool, cx: &mut Context<Self>) {
        self.minutes_full = full;
        if full {
            self.minutes_collapsed = false;
        }
        cx.notify();
    }

    /// 全屏阅读: the 纪要 takes the whole reading area; 返回正文 goes back.
    pub fn open_minutes(&mut self, cx: &mut Context<Self>) {
        self.insights.view = View::Minutes;
        cx.notify();
    }

    /// Switches between the screen sans-serif and the print-like serif.
    pub fn set_reading_font(&mut self, font: memex_client::ReadingFont, cx: &mut Context<Self>) {
        memex_client::set_reading_font(font);
        self.list.remeasure();
        cx.emit(ReaderEvent::FontChanged(font));
        cx.notify();
    }

    /// The 古籍 styles remembered per item (from the shell's preferences).
    pub fn set_item_styles(&mut self, styles: impl IntoIterator<Item = (String, memex_client::ItemStyle)>) {
        self.item_styles = styles.into_iter().collect();
    }

    /// The 古籍 display of the item on screen (defaults when nothing is chosen).
    pub fn item_style(&self) -> memex_client::ItemStyle {
        self.model.detail().and_then(|d| self.item_styles.get(&d.canonical.item_id)).copied().unwrap_or_default()
    }

    /// The typeface the item on screen is drawn with.
    pub fn item_font(&self) -> memex_client::ReadingFont {
        self.item_style().font(memex_client::reading_font())
    }

    pub fn set_item_style(&mut self, style: memex_client::ItemStyle, cx: &mut Context<Self>) {
        let Some(id) = self.model.detail().map(|d| d.canonical.item_id.clone()) else { return };
        let before = self.item_style();
        if before == style { return; }
        if style.is_default() { self.item_styles.remove(&id); } else { self.item_styles.insert(id.clone(), style); }
        if before.vertical != style.vertical { self.vscroll = 0.; }
        self.vcols = None;
        self.list.remeasure();
        self.list_o.remeasure();
        self.list_t.remeasure();
        cx.emit(ReaderEvent::ItemStyleChanged(id, style));
        cx.notify();
    }

    /// 竖排: reads on by `by` pixels (negative: back towards the right edge).
    pub fn scroll_vertical(&mut self, by: f32, cx: &mut Context<Self>) {
        self.vscroll = (self.vscroll + by).max(0.);
        cx.notify();
    }

    /// 简繁: 原文 / 简体 / 繁体 of the item on screen (display only).
    pub fn set_script(&mut self, script: memex_client::opencc::Script, cx: &mut Context<Self>) {
        let style = memex_client::ItemStyle { script, ..self.item_style() };
        self.set_item_style(style, cx);
    }

    /// 竖排 / 横排 of the item on screen.
    pub fn set_vertical(&mut self, vertical: bool, cx: &mut Context<Self>) {
        let style = memex_client::ItemStyle { vertical, ..self.item_style() };
        self.set_item_style(style, cx);
    }

    /// Aa: an item with a 古籍 style keeps its own typeface; others switch the app's.
    fn next_font(&mut self, cx: &mut Context<Self>) {
        let style = self.item_style();
        if style.is_default() {
            self.set_reading_font(memex_client::reading_font().other(), cx);
        } else {
            self.set_item_style(memex_client::ItemStyle { font: Some(self.item_font().other()), ..style }, cx);
        }
    }

    pub fn set_reading_size(&mut self, size: memex_client::ReadingSize, cx: &mut Context<Self>) {
        self.reading_size = size;
        self.list.remeasure();
        cx.emit(ReaderEvent::SizeChanged(size));
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        // A chapter draft belongs to one side: switching sides waits for it.
        if self.editing.chapter.is_some() && self.model.mode() != mode {
            if self.blocks_navigation(cx) { return; }
            self.editing = Default::default();
        }
        self.model.set_mode(mode);
        self.pass_mode(mode);
        self.list.reset(self.model.layout().rows.len() + LEAD);
        self.refresh_format(cx);
        cx.emit(ReaderEvent::ModeChanged(mode));
        cx.notify();
    }

    /// After an interface language switch; keeps the item and scroll position.
    pub fn relocalize(&mut self, cx: &mut Context<Self>) {
        self.model.relocalize();
        self.list.remeasure();
        cx.notify();
    }

    /// Scrolls to the last block (End).
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.list.scroll_to_end();
        cx.notify();
    }

    /// Where the 纪要 button was drawn in the last frame (`None`: not shown).
    pub fn minutes_button_bounds(&self) -> Option<Bounds<Pixels>> {
        self.minutes_button.get()
    }

    pub fn progress_rail_bounds(&self) -> Option<Bounds<Pixels>> {
        self.rail_bounds.get()
    }

    pub fn reading_position(&self) -> u8 {
        list_reading_percent(&self.list)
    }

    /// The 纪要 button hid the finished card (manual minutes).
    pub fn minutes_card_hidden(&self) -> bool {
        self.minutes_hidden
    }

    /// Set by the shell from the height it gives the reader.
    pub fn set_compact(&mut self, compact: bool, cx: &mut Context<Self>) {
        if self.compact != compact {
            self.compact = compact;
            cx.notify();
        }
    }

    /// Scrolls the article so that list row `row` is at the top (row 0 is the
    /// 纪要 card); used by the development walk.
    pub fn scroll_rows(&mut self, row: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
        cx.notify();
    }

    /// `Some(true)` once the last block is fully visible.
    pub fn at_end(&self) -> Option<bool> {
        self.list.is_scrolled_to_end()
    }

    /// Reads the cited slice from the agent, then highlights it in the original.
    pub fn jump(&mut self, citation: Citation, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let c = citation.clone();
        let task = cx.background_executor().spawn(async move {
            client.context_read(ContextRequest {
                item_id: c.item_id,
                expected_revision_id: c.revision_id,
                start_byte: c.start_byte,
                end_byte: c.end_byte,
                max_sensitivity: Sensitivity::Restricted,
            })
        });
        cx.spawn(async move |this, cx| {
            let slice = task.await;
            this.update(cx, |this, cx| {
                if let JumpOutcome::Highlight { block, .. } = this.model.resolve(&citation, slice) {
                    this.list.reset(this.model.layout().rows.len() + LEAD);
                    // Rows follow block indexes in 原文 and 对照. Heights are
                    // unmeasured after a reset, so scroll by index, not by reveal.
                    this.list.scroll_to(ListOffset { item_ix: block.saturating_sub(1) + LEAD, offset_in_item: px(0.) });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The 翻译 button fixed in the 对照 / 译文 toolbar: the item (or the book's
    /// chapter on screen) as it stands; disabled states say why.
    fn translate_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::translate::TrButton;
        if self.model.mode() == Mode::Original { return None; }
        let base = |label: String| Button::new("translate-toolbar").small().outline().icon(IconName::Globe).label(label);
        if self.model.book().is_some() {
            let (label, action) = self.chapter_tr_button();
            return Some(match action {
                Some(a) => base(label).on_click(cx.listener(move |r, _, _, cx| r.press_chapter_tr(a, cx))).into_any_element(),
                None => base(label).disabled(true).into_any_element(),
            });
        }
        Some(match self.translate.button() {
            TrButton::Request(label) => base(label.into()).on_click(cx.listener(|r, _, _, cx| r.request_translation(cx))).into_any_element(),
            TrButton::Recheck(why) => base(tx("重试").into()).tooltip(why).on_click(cx.listener(|r, _, _, cx| r.recheck_translation(cx))).into_any_element(),
            TrButton::Settings => base(tx("去设置翻译模型").into()).on_click(cx.listener(|_, _, _, cx| cx.emit(ReaderEvent::OpenSettings))).into_any_element(),
            // Running: a thin bar in the theme's (rose) primary moves under the label.
            TrButton::Running(label) if self.translate.is_running() => {
                let color = cx.theme().primary;
                div()
                    .relative()
                    .child(base(label).disabled(true))
                    .child(
                        div().absolute().left(px(4.)).right(px(4.)).bottom(px(1.)).h(px(2.)).overflow_hidden().rounded_full().bg(color.opacity(0.15)).child(
                            div().absolute().top_0().h_full().w(relative(0.35)).rounded_full().bg(color).with_animation(
                                "translate-running",
                                Animation::new(std::time::Duration::from_millis(1600)).repeat().with_easing(ease_in_out),
                                |d, delta| d.left(relative(delta * 1.35 - 0.35)),
                            ),
                        ),
                    )
                    .into_any_element()
            }
            TrButton::Running(label) => base(label).disabled(true).into_any_element(),
            TrButton::Done => base(tx("已翻译").into()).disabled(true).into_any_element(),
            TrButton::Reason(why) => base(why).disabled(true).into_any_element(),
        })
    }

    /// 重试 after the translation state could not be read: read it first; only
    /// a state that is neither running nor finished sends the request.
    pub fn recheck_translation(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.model.active_item().map(str::to_string) else { return };
        self.poll_ticket += 1;
        let ticket = self.poll_ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.library_item_read(&item_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.poll_ticket != ticket { return; }
                match this.translate.on_recheck(result) {
                    Some(next) => this.follow(next, cx),
                    None => this.request_translation(cx),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// 翻译此文 / 生成纪要, or the way to settings when no model is configured.
    fn offer(&self, id: &'static str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.translate.can_request() {
            let label = self.translate.action_label();
            return Some(
                Button::new(id)
                    .small()
                    .primary()
                    .icon(if label == tx("翻译此文") { IconName::Globe } else if label == tx("重试") { IconName::RefreshCw } else { IconName::BookOpen })
                    .label(label)
                    .on_click(cx.listener(|this, _, _, cx| this.request_translation(cx)))
                    .into_any_element(),
            );
        }
        self.translate.needs_provider().then(|| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(IconName::Settings)
                .label(tx("去设置翻译模型"))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ReaderEvent::OpenSettings)))
                .into_any_element()
        })
    }

    /// The AI 纪要 card between the title block and the body: the text when the
    /// minutes succeeded, one line while generating or after a failure, nothing
    /// when not requested, unavailable, or not reported by the agent.
    fn minutes_card(&self, serif: SharedString, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let manual = self.translate.manual_minutes();
        let retry = |cx: &mut Context<Self>| {
            Button::new("minutes-retry").small().outline().icon(IconName::RefreshCw).label(tx("重试")).on_click(cx.listener(|this, _, _, cx| this.press_minutes(cx))).into_any_element()
        };
        // Manual minutes: the request's own outcome comes first.
        let (text, line, failed, action): (Option<String>, Option<String>, bool, Option<AnyElement>) = match self.translate.minutes_ask() {
            MinutesAsk::Requesting => (None, Some(tx("正在请求纪要…").into()), false, None),
            MinutesAsk::NoProvider => (
                None,
                Some(tx("还没有配置纪要模型").into()),
                true,
                Some(Button::new("minutes-settings").small().outline().icon(IconName::Settings).label(tx("去设置")).on_click(cx.listener(|_, _, _, cx| cx.emit(ReaderEvent::OpenSettings))).into_any_element()),
            ),
            MinutesAsk::Error(message) => (None, Some(message.clone()), true, Some(retry(cx))),
            MinutesAsk::Idle => {
                let minutes: &AiMinutes = self.translate.minutes()?;
                let text = minutes.text().map(str::to_string);
                match (&minutes.state, &text) {
                    // Hidden with the 纪要 button.
                    (MinutesState::Succeeded, Some(_)) if manual && self.minutes_hidden => return None,
                    (MinutesState::Succeeded, Some(_)) => (text, None, false, None),
                    (MinutesState::Queued | MinutesState::Running, _) => (None, minutes.state_line(), false, None),
                    (MinutesState::Failed, _) => (None, minutes.state_line(), true, manual.then(|| retry(cx))),
                    _ => return None,
                }
            }
        };
        let collapsed = self.minutes_collapsed && text.is_some();
        let full = self.minutes_full;
        let dark = theme.background.l < 0.5;
        let rose = theme.primary;
        let header = h_flex()
            .id("minutes-header")
            .gap_2()
            .items_center()
            .child(div().px(px(5.)).rounded(px(4.)).bg(rose.opacity(0.16)).text_color(rose).text_xs().font_weight(FontWeight::SEMIBOLD).child("AI"))
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(theme.foreground).child(tx("AI 纪要")))
            .when_some(line.clone(), |d, l| {
                d.child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(if failed { theme.warning } else { theme.muted_foreground })
                        .when(!failed, |d| d.child(Icon::new(IconName::Loader).xsmall().text_color(rose)))
                        .when(failed, |d| d.child(Icon::new(IconName::TriangleAlert).xsmall()))
                        .child(l),
                )
            })
            .children(action)
            .child(div().flex_1())
            .when(text.is_some() && !collapsed, |d| {
                d.child(
                    Button::new("minutes-full")
                        .ghost()
                        .xsmall()
                        .icon(if full { IconName::ChevronUp } else { IconName::ChevronsUpDown })
                        .label(if full { tx("收回") } else { tx("展开全部") })
                        .on_click(cx.listener(move |this, _, _, cx| this.set_minutes_full(!full, cx))),
                )
                .child(
                    Button::new("minutes-read")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Maximize)
                        .label(tx("全屏阅读"))
                        .on_click(cx.listener(|this, _, _, cx| this.open_minutes(cx))),
                )
            })
            .when(text.is_some(), |d| {
                d.cursor_pointer()
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if collapsed { tx("展开") } else { tx("收起") })
                            .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_minutes_collapsed(!collapsed, cx)))
            });
        Some(
            v_flex()
                .mb_3()
                .max_w(px(720.))
                .px_4()
                .py_3()
                .gap_2()
                .rounded(px(10.))
                .border_1()
                .border_color(rose.opacity(if dark { 0.30 } else { 0.22 }))
                .bg(rose.opacity(if dark { 0.10 } else { 0.05 }))
                .child(header)
                .when_some(text.filter(|_| !collapsed), |d, t| {
                    d.child(
                        div()
                            .id("minutes-body")
                            .when(!full, |d| d.max_h(px(220.)).overflow_y_scroll())
                            .font_family(serif)
                            .text_sm()
                            .line_height(relative(1.75))
                            .text_color(theme.foreground)
                            .child(TextView::markdown("minutes-text", memex_client::markdown::safe_markdown(&crate::inline::upright_markdown(&memex_client::minutes_markdown(&t)))).selectable(true)),
                    )
                })
                .into_any_element(),
        )
    }

    /// 重点标注 / 金句 / 事实核查 with their state; none for an agent without them.
    fn insight_buttons(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.capability_known {
            return Some(Button::new("retry-reader-capabilities").small().outline()
                .label(if self.capability_probe_running { "正在连接 AI 功能…" } else { "重试连接 AI 功能" })
                .disabled(self.capability_probe_running)
                .on_click(cx.listener(|this, _, _, cx| this.probe_capability(cx)))
                .into_any_element());
        }
        let minutes = self.translate.minutes_button();
        if !self.insights.visible() && minutes.is_none() {
            return None;
        }
        let border = cx.theme().border;
        let mut row = h_flex().gap_1().items_center().child(div().w(px(1.)).h(px(18.)).mx_1().bg(border));
        // 纪要 first in the AI group (manual minutes only).
        if let Some(state) = minutes {
            let shown = state == ButtonState::Done && !self.minutes_hidden && self.insights.view == View::Text;
            // Keyed by state: GPUI keeps a tooltip alive while the pointer
            // rests, so a stable id would keep showing the old state's text.
            let button = Button::new(("insight-minutes", tooltip_key(&state) * 2 + shown as usize))
                .small()
                .label(tx("纪要"))
                .accessibility_label(format!("{} · {}", tx("纪要"), state.label()))
                .child(ai_status_dot(&state, cx))
                .when(shown, |b| b.primary())
                .when(!shown, |b| b.outline())
                .tooltip(match &state {
                    ButtonState::Done if shown => tx("隐藏纪要卡片").to_string(),
                    ButtonState::Done => tx("显示纪要卡片").to_string(),
                    ButtonState::Failed(reason) | ButtonState::Stale(reason) => reason.clone(),
                    ButtonState::Generating => tx("纪要生成中…").to_string(),
                    ButtonState::NotGenerated => tx("生成这篇的 AI 纪要").to_string(),
                });
            let button = button.on_click(cx.listener(|this, _, _, cx| {
                if this.insights.view != View::Text && this.translate.minutes_button() == Some(ButtonState::Done) {
                    // From 金句 / 事实核查: back to the text with the card shown.
                    this.insights.view = View::Text;
                    this.minutes_hidden = false;
                    cx.notify();
                } else {
                    this.press_minutes(cx);
                }
            }));
            let probe = self.minutes_button.clone();
            row = row.child(div().relative().child(button).child(canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full()));
        }
        if !self.insights.visible() {
            return Some(row.into_any_element());
        }
        for (kind, id) in [(InsightKind::Highlights, "insight-highlights"), (InsightKind::Quotes, "insight-quotes"), (InsightKind::FactCheck, "insight-facts")] {
            if !self.insights.available(kind) {
                continue;
            }
            let state = self.insights.state(kind);
            let active = match kind {
                InsightKind::Highlights => self.insights.emphasis_shown() && self.insights.view == View::Text,
                InsightKind::Quotes => self.insights.view == View::Quotes,
                InsightKind::FactCheck => self.insights.view == View::FactCheck,
            };
            let button = Button::new((id, tooltip_key(&state)))
                .small()
                .label(kind.label())
                .accessibility_label(format!("{} · {}", kind.label(), state.label()))
                .child(ai_status_dot(&state, cx))
                .when(active, |b| b.primary())
                .when(!active, |b| b.outline());
            let button = match &state {
                ButtonState::Failed(reason) | ButtonState::Stale(reason) => button.tooltip(reason.clone()),
                _ => button.tooltip(state.label()),
            };
            row = row.child(button.on_click(cx.listener(move |this, _, _, cx| this.press_insight(kind, cx))));
        }
        Some(row.into_any_element())
    }

    /// 重点导航 (the Mac counterpart of the phone's scrubber dial): ticks on a
    /// thin rail at the article's right edge, one per mark of the side being
    /// read, placed by row; hovering or clicking the rail lists the marks, and
    /// a click jumps to one. Positions are by row, not by pixel: the list is
    /// virtualized, so rows not laid out yet have no height to measure.
    #[allow(dead_code)]
    fn nav_rail(&self, marks: Rc<Vec<AlongMark>>, rows: usize, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.insights.view != View::Text || rows == 0 {
            return None;
        }
        let theme = cx.theme().clone();
        let tick = |s: &memex_client::insights::HighlightStyle| match s {
            memex_client::insights::HighlightStyle::Bold => p.fg.opacity(0.7),
            memex_client::insights::HighlightStyle::Highlight => hsla(44. / 360., 0.85, 0.52, 1.),
            memex_client::insights::HighlightStyle::Positive => p.pos,
            memex_client::insights::HighlightStyle::Negative => p.neg,
            memex_client::insights::HighlightStyle::Other(_) => p.muted,
        };
        let frac = |row: usize| (row as f32 + 0.5) / rows as f32;
        let open = !marks.is_empty() && (self.nav_hover || self.nav_pinned);
        let hover_marks = marks.clone();
        let count = marks.len();
        let bounds = self.rail_bounds.clone();
        let dragging = self.rail_dragging.clone();
        let scroll = self.list.clone();
        let thumb_top = bounds.get().map_or(px(0.), |b| (b.size.height - px(24.)).max(px(0.)) * (self.reading_pct as f32 / 100.));
        let mut rail = div()
            .id("nav-rail")
            .relative()
            .flex_none()
            .w(px(12.))
            .h_full()
            .rounded(px(6.))
            .bg(theme.muted.opacity(if open { 0.7 } else { 0.35 }))
            .cursor_pointer()
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                let Some(b) = this.rail_bounds.get() else { return; };
                let f = ((e.position.y - b.top()) / b.size.height.max(px(1.))).clamp(0., 1.);
                this.nav_anchor = f;
                this.nav_index = hover_marks.iter().enumerate().min_by(|(_, a), (_, b)| {
                    ((a.row as f32 + 0.5) / rows as f32 - f).abs().total_cmp(&((b.row as f32 + 0.5) / rows as f32 - f).abs())
                }).map_or(0., |(k, _)| k as f32);
                this.nav_hover = true;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.rail_dragging.set(true);
                if let Some(bounds) = this.rail_bounds.get() { seek_reading_rail(&this.list, bounds, e.position.y); }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(div().absolute().left(px(2.)).w(px(8.)).h(px(24.)).top(thumb_top).rounded(px(4.)).bg(theme.muted_foreground.opacity(0.65)))
            .child(canvas(move |b, _, _| { bounds.set(Some(b)); b }, move |b, _, window, _| {
                let moving = dragging.clone();
                let scroll = scroll.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase.bubble() && moving.get() && e.dragging() {
                        seek_reading_rail(&scroll, b, e.position.y);
                        window.refresh();
                        cx.stop_propagation();
                    }
                });
                let moving = dragging.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, _| {
                    if phase.bubble() && e.button == MouseButton::Left && moving.replace(false) { window.refresh(); }
                });
            }).absolute().top_0().left_0().size_full());
        for m in marks.iter() {
            rail = rail.child(div().absolute().left(px(2.)).w(px(8.)).h(px(3.)).rounded(px(1.5)).top(relative(frac(m.row))).bg(tick(&m.style)));
        }
        let both_sides = marks.iter().any(|m| m.side == Side::Translation) && marks.iter().any(|m| m.side == Side::Original);
        let list_el = open.then(|| {
            let height = self.rail_bounds.get().map_or(380., |b| f32::from(b.size.height));
            let margin = 74_f32.min(height / 2.);
            let pivot = (height * self.nav_anchor).clamp(margin, height - margin);
            let center = self.nav_index.clamp(0., count.saturating_sub(1) as f32);
            let selected = center.round() as usize;
            let mut col = div()
                .id("nav-list")
                .relative()
                .w(px(300.))
                .h_full()
                .overflow_hidden()
                .rounded(px(8.))
                .bg(theme.popover.opacity(0.96))
                .text_sm()
                .child(div().absolute().top_0().left_0().px_2().py_1().text_xs().text_color(theme.muted_foreground)
                    .child(format!("{} · {}/{}", tx("重点导航"), selected + 1, count)));
            for (k, m) in marks.iter().enumerate() {
                let distance = k as f32 - center;
                let top = pivot + distance * 40. - 17.;
                if top < 26. || top + 34. > height { continue; }
                let (row, block) = (m.row, m.block);
                col = col.child(
                    h_flex()
                        .id(("nav-item", k))
                        .absolute().top(px(top)).right(px(8. + (distance * distance * 2.).min(70.)))
                        .max_w(px(270.)).h(px(34.))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(px(10.))
                        .border_1().border_color(theme.border)
                        .bg(if k == selected { theme.primary } else { theme.background })
                        .text_color(if k == selected { theme.primary_foreground } else { theme.foreground })
                        .text_size(px((16. - distance.abs() * 0.5).max(12.)))
                        .hover(|d| d.bg(if k == selected { theme.primary } else { theme.secondary }))
                        .cursor_pointer()
                        .child(div().flex_none().size(px(7.)).rounded_full().bg(tick(&m.style)))
                        .child(div().flex_1().min_w_0().truncate().child(m.text.clone()))
                        .when(both_sides, |d| d.child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(if m.side == Side::Original { tx("原") } else { tx("译") })))
                        .child(div().flex_none().text_xs().text_color(if k == selected { theme.primary_foreground } else { theme.muted_foreground }).child(format!("{}%", (frac(m.row) * 100.).round() as u32)))
                        .when_some(m.label.clone(), |d, l| d.tooltip(move |window, cx| Tooltip::new(l.clone()).build(window, cx)))
                        .on_click(cx.listener(move |this, _, _, cx| this.jump_to_mark(row, block, cx))),
                );
            }
            col
        });
        Some(
            h_flex()
                .id("nav")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-18.))
                .items_start()
                .gap_1()
                .on_scroll(cx.listener(move |this, e: &ScrollEvent, _, cx| {
                    this.nav_index = (this.nav_index - e.delta.pixel_delta(px(20.)).y / px(40.)).clamp(0., count.saturating_sub(1) as f32);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if this.nav_hover != *hovered {
                        this.nav_hover = *hovered;
                        cx.notify();
                    }
                }))
                .when_some(list_el, |d, l| d.child(l))
                .child(rail)
                .into_any_element(),
        )
    }

    /// The line under the toolbar while 重点标注 is on: the legend, or why it is not shown.
    fn emphasis_line(&self, shown: Mode, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.insights.emphasis || self.insights.view != View::Text {
            return None;
        }
        let theme = cx.theme();
        let line = |icon: IconName, text: String, warn: bool| {
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(if warn { theme.warning } else { theme.muted_foreground })
                .child(Icon::new(icon).xsmall())
                .child(text)
                .into_any_element()
        };
        match self.insights.state(InsightKind::Highlights) {
            ButtonState::Failed(reason) => return Some(line(IconName::TriangleAlert, txf("重点标注生成失败：{0}", &[&reason]), true)),
            ButtonState::Stale(reason) => {
                return Some(
                    h_flex()
                        .gap_2()
                        .child(line(IconName::TriangleAlert, reason, true))
                        .child(Button::new("highlights-redo").ghost().xsmall().label(tx("重新生成")).on_click(cx.listener(|this, _, _, cx| this.retry_insight(InsightKind::Highlights, cx))))
                        .into_any_element(),
                )
            }
            _ => {}
        }
        self.insights.highlights()?;
        // `original_only`: the translation side has no marks; say so instead of the legend.
        if shown != Mode::Original && !self.insights.translation_marked() {
            let note = if shown == Mode::Translation { tx("译文这一侧还没有重点标注，只有原文有；请切换到原文或对照") } else { tx("译文这一侧还没有重点标注，只标在原文上") };
            return Some(line(IconName::Info, note.into(), false));
        }
        let mut text = String::new();
        let mut runs = vec![];
        let styles = [
            (tx("加粗"), tx("要点"), HighlightStyle { font_weight: Some(FontWeight::BOLD), color: Some(p.fg), ..Default::default() }),
            (tx("黄底"), tx("关键信息"), HighlightStyle { background_color: Some(p.hl_bg), color: Some(p.fg), ..Default::default() }),
            (tx("蓝"), tx("正面"), HighlightStyle { color: Some(p.pos), ..Default::default() }),
            (tx("红"), tx("负面"), HighlightStyle { color: Some(p.neg), ..Default::default() }),
        ];
        for (i, (key, meaning, style)) in styles.into_iter().enumerate() {
            if i > 0 {
                text.push_str(" · ");
            }
            let start = text.len();
            text.push_str(key);
            runs.push((start..text.len(), style));
            text.push('=');
            text.push_str(meaning);
        }
        Some(
            h_flex()
                .gap_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(StyledText::new(text).with_highlights(runs))
                .child(div().text_color(theme.muted_foreground.opacity(0.8)).child(tx("AI 生成，仅供参考")))
                .when_some(self.picked.clone(), |d, l| d.child(div().px_2().rounded(px(4.)).bg(theme.secondary).text_color(theme.foreground).child(txf("标注：{0}", &[&l]))))
                .into_any_element(),
        )
    }

    /// 金句 / 事实核查 while not done: generating, failed (with 重试), or not generated.
    fn insight_waiting(&self, kind: InsightKind, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let (icon, text, action) = match self.insights.state(kind) {
            ButtonState::Done => return None,
            ButtonState::Generating => (IconName::Loader, txf("{0}生成中…", &[&kind.label()]), None),
            ButtonState::Failed(reason) => (IconName::TriangleAlert, txf("{0}生成失败：{1}", &[&kind.label(), &reason]), Some(tx("重试"))),
            ButtonState::Stale(reason) => (IconName::TriangleAlert, reason, Some(tx("重新生成"))),
            ButtonState::NotGenerated => (IconName::Info, txf("{0}尚未生成", &[&kind.label()]), Some(tx("生成"))),
        };
        Some(
            v_flex()
                .flex_1()
                .max_w(px(720.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(h_flex().gap_2().child(Icon::new(icon).small().text_color(theme.primary)).child(text))
                .when_some(action, |d, label| {
                    d.child(Button::new("insight-retry").small().outline().label(label).on_click(cx.listener(move |this, _, _, cx| this.retry_insight(kind, cx))))
                })
                .into_any_element(),
        )
    }

    fn locate_button(id: (&'static str, usize), block: usize, cx: &mut Context<Self>) -> Button {
        Button::new(id).ghost().xsmall().icon(IconName::Search).label(tx("定位原文")).on_click(cx.listener(move |this, _, _, cx| this.locate(block, cx)))
    }

    /// 金句: one card per quote, the verbatim words large, the Chinese beneath.
    fn quotes_panel(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if let Some(waiting) = self.insight_waiting(InsightKind::Quotes, cx) {
            return waiting;
        }
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let quotes = self.insights.quotes().unwrap_or_default().to_vec();
        let mut col = v_flex().id("quotes-panel").flex_1().min_h_0().overflow_y_scroll().gap_3().pb_6();
        if quotes.is_empty() {
            col = col.child(div().text_sm().text_color(theme.muted_foreground).child(tx("没有找到金句")));
        }
        for (i, q) in quotes.iter().enumerate() {
            col = col.child(
                v_flex()
                    .max_w(px(720.))
                    .gap_2()
                    .px_5()
                    .py_4()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(p.rose.opacity(if dark { 0.28 } else { 0.20 }))
                    .bg(p.rose.opacity(if dark { 0.08 } else { 0.04 }))
                    .child(div().font_family(p.serif.clone()).text_xl().line_height(relative(1.6)).text_color(p.fg).child(format!("“{}”", q.quote)))
                    .when_some(q.translation_zh.clone().filter(|t| !t.trim().is_empty()), |d, t| {
                        d.child(div().font_family(p.serif.clone()).text_base().line_height(relative(1.7)).text_color(p.warm).child(t))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().text_xs().text_color(p.muted).child(q.why.clone()))
                            .child(Self::locate_button(("quote-locate", i), q.block_index, cx)),
                    ),
            );
        }
        col.into_any_element()
    }

    /// Under the toolbar while 简繁 or 竖排 is on: what it changes and what is off meanwhile.
    fn guji_line(&self, mode: Mode, missing_translation: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let style = self.item_style();
        if style.script == memex_client::opencc::Script::Original && !style.vertical { return None; }
        let mut parts: Vec<String> = vec![];
        if style.script != memex_client::opencc::Script::Original { parts.push(tx("转换只影响显示，不修改原文").to_string()); }
        if style.vertical && mode == Mode::Bilingual && !missing_translation { parts.push(tx("竖排只显示原文；对照请切回横排").to_string()); }
        parts.push(tx("此时不能选择文字或划线").to_string());
        Some(div().text_xs().text_color(cx.theme().muted_foreground).child(parts.join(" · ")).into_any_element())
    }

    /// 竖排: the side's blocks as 古籍 columns, right to left, scrolled sideways.
    /// Only the columns in view are built; the layout is redone when the text,
    /// 简繁 or the strip's height changes.
    fn vertical_panel(&mut self, translation_side: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        use crate::vertical::{columns, max_scroll, visible, ColKind, Source};
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let style = self.item_style();
        let fs = (p.body_size + 2.).round();
        let cell = (fs * 1.2).round();
        let col = (fs * 2.2).round();
        let pad = 18.;
        let (w, h) = self.vbounds.get().map(|b| (f32::from(b.size.width), f32::from(b.size.height))).unwrap_or((800., 560.));
        // One cell is kept free at the foot for a hanging closing mark.
        let per = (((h - 2. * pad) / cell).floor() as usize).saturating_sub(1).max(4);
        let (item, revision) = self.model.detail().map(|d| (d.canonical.item_id.clone(), d.canonical.revision_id.clone())).unwrap_or_default();
        let blocks = if translation_side { self.model.translation() } else { self.model.original() };
        let key = VKey(item, revision, self.model.book().map(|b| b.at), translation_side, style.script, per, blocks.len());
        if self.vcols.as_ref().is_none_or(|(k, _)| *k != key) {
            // Another text starts again at its beginning (the right edge); a new height keeps the place roughly.
            let same_text = self.vcols.as_ref().is_some_and(|(k, _)| (&k.0, &k.1, k.2, k.3) == (&key.0, &key.1, key.2, key.3));
            if !same_text { self.vscroll = 0.; }
            let sources: Vec<Source> = blocks.iter().filter(|b| !matches!(b.kind, BlockKind::Image | BlockKind::Code))
                .map(|b| Source { text: &b.text, bold: &b.bold, red: &b.marks.red, heading_len: if matches!(b.kind, BlockKind::Heading(_)) { b.heading_len } else { 0 } })
                .collect();
            self.vcols = Some((key, Rc::new(columns(&sources, per, style.script))));
        }
        let cols = self.vcols.as_ref().map(|(_, c)| c.clone()).unwrap_or_default();
        let n = cols.len();
        let most = max_scroll(n, w, pad, col);
        self.vscroll = self.vscroll.clamp(0., most);
        let scroll = self.vscroll;
        let rule = p.warm.opacity(if dark { 0.22 } else { 0.18 });
        let paper = if dark { hsla(30. / 360., 0.10, 0.12, 1.) } else { hsla(38. / 360., 0.38, 0.965, 1.) };
        let mut strip = div().id("vertical-strip").relative().flex_1().min_h_0().w_full().overflow_hidden().rounded(px(6.)).bg(paper)
            .border_1().border_color(rule)
            .on_scroll(cx.listener(move |this, e: &ScrollEvent, _, cx| {
                // Down (or a swipe towards the left) reads on: the strip moves right.
                let d = e.delta.pixel_delta(px(cell));
                this.vscroll = (this.vscroll + f32::from(d.x) - f32::from(d.y)).clamp(0., most);
                cx.notify();
            }));
        for i in visible(n, w, scroll, pad, col) {
            let c = &cols[i];
            let right = w - pad + scroll - i as f32 * col;
            let runs: Vec<(Range<usize>, HighlightStyle)> = c.runs.iter().map(|(r, m)| (r.clone(), HighlightStyle {
                font_weight: m.bold.then_some(FontWeight::BOLD),
                color: if m.red { Some(p.neg) } else if c.kind == ColKind::Heading { Some(p.rose) } else { None },
                ..Default::default()
            })).collect();
            strip = strip.child(div().absolute().top_0().bottom_0().left(px(right - col)).w(px(col)).border_l_1().border_color(rule).when(i == 0, |d| d.border_r_1())
                .flex().justify_center()
                .when(!c.text.is_empty(), |d| d.child(div().mt(px(pad)).w(px(fs * 1.3)).text_center().font_family(p.serif.clone()).text_size(px(fs)).line_height(px(cell)).text_color(p.fg)
                    .child(StyledText::new(c.text.clone()).with_highlights(runs)))));
        }
        let bounds = self.vbounds.clone();
        strip.child(canvas(move |b, window, _| { if bounds.get() != Some(b) { bounds.set(Some(b)); window.refresh(); } }, |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .into_any_element()
    }

    /// The reading typeface switch: "Aa" with the current choice as tooltip.
    fn font_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let font = self.item_font();
        let own = !self.item_style().is_default();
        Button::new("reading-font")
            .ghost()
            .small()
            .label("Aa")
            .tooltip(memex_client::txf(if own { "本条阅读字体：{0}（点击切换）" } else { "阅读字体：{0}（点击切换）" }, &[&font.label()]))
            .on_click(cx.listener(move |this, _, _, cx| this.next_font(cx)))
            .into_any_element()
    }

    /// 简繁 (原文 / 简体 / 繁体) and 竖排 / 横排 of the item on screen.
    fn guji_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        use memex_client::opencc::Script;
        let theme = cx.theme();
        let style = self.item_style();
        let mut seg = h_flex().id("script-switch").rounded(px(7.)).bg(theme.muted).p(px(2.))
            .tooltip(|window, cx| Tooltip::new(format!("{} · {}", tx("简繁"), tx("转换只影响显示，不修改原文"))).build(window, cx));
        for (s, id) in [(Script::Original, "script-original"), (Script::Simplified, "script-simplified"), (Script::Traditional, "script-traditional")] {
            seg = seg.child(Button::new(id).small().label(s.label())
                .when(style.script == s, |b| b.primary()).when(style.script != s, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| this.set_script(s, cx))));
        }
        let vertical = style.vertical;
        h_flex().gap_2().items_center()
            .child(h_flex().gap_1().items_center().child(div().text_xs().text_color(theme.muted_foreground).child(tx("简繁"))).child(seg))
            .child(Button::new("vertical-toggle").small().ghost().label(tx(if vertical { "横排" } else { "竖排" }))
                .tooltip(tx(if vertical { "切回横排" } else { "竖排：从右往左阅读" }))
                .on_click(cx.listener(move |this, _, _, cx| this.set_vertical(!vertical, cx))))
            .into_any_element()
    }

    fn size_button(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("reading-size").ghost().small()
            .label(format!("A {}", self.reading_size.pixels() as u8))
            .tooltip(tx("字号：点击切换 16 / 18 / 20 / 22"))
            .on_click(cx.listener(|this, _, _, cx| this.set_reading_size(this.reading_size.next(), cx)))
            .into_any_element()
    }

    /// AI 纪要 in the whole reading area: wider measure, body-size type.
    fn minutes_panel(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let minutes = self.translate.minutes();
        let text = minutes.and_then(|m| m.text()).map(str::to_string);
        let back = Button::new("minutes-back").ghost().small().icon(IconName::ArrowLeft).label(tx("返回正文")).on_click(cx.listener(|this, _, _, cx| this.set_insight_view(View::Text, cx)));
        let top = h_flex()
            .gap_2()
            .items_center()
            .pb_2()
            .child(div().px(px(5.)).rounded(px(4.)).bg(p.rose.opacity(0.16)).text_color(p.rose).text_xs().font_weight(FontWeight::SEMIBOLD).child("AI"))
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(tx("AI 纪要")))
            .child(div().flex_1())
            .child(back);
        let body = match text {
            Some(t) => div()
                .max_w(px(880.))
                .font_family(p.serif.clone())
                .text_lg()
                .line_height(relative(1.9))
                .text_color(p.fg)
                .child(TextView::markdown("minutes-full-text", memex_client::markdown::safe_markdown(&crate::inline::upright_markdown(&memex_client::minutes_markdown(&t)))).selectable(true))
                .into_any_element(),
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(minutes.and_then(|m| m.state_line()).unwrap_or_else(|| tx("还没有纪要").into()))
                .into_any_element(),
        };
        v_flex().id("minutes-panel").flex_1().min_h_0().overflow_y_scroll().pb_6().child(top).child(body).into_any_element()
    }

    /// 事实核查: each claim with its verdict, confidence and explanation.
    fn facts_panel(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if let Some(waiting) = self.insight_waiting(InsightKind::FactCheck, cx) {
            return waiting;
        }
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let facts = self.insights.facts().unwrap_or_default().to_vec();
        let mut col = v_flex().id("facts-panel").flex_1().min_h_0().overflow_y_scroll().gap_3().pb_6();
        if self.insights.model_knowledge() {
            col = col.child(
                h_flex()
                    .max_w(px(720.))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .bg(theme.secondary)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Info).xsmall().text_color(theme.primary))
                    .child(tx("基于模型知识，未联网核查"))
                    .child(div().text_color(theme.muted_foreground.opacity(0.8)).child(tx("AI 生成，仅供参考"))),
            );
        }
        if facts.is_empty() {
            col = col.child(div().text_sm().text_color(theme.muted_foreground).child(tx("没有可核查的陈述")));
        }
        for (i, f) in facts.iter().enumerate() {
            let color = p.verdict(&f.verdict, dark);
            col = col.child(
                v_flex()
                    .max_w(px(720.))
                    .gap_2()
                    .px_4()
                    .py_3()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(color.opacity(if dark { 0.38 } else { 0.30 }))
                    .bg(color.opacity(if dark { 0.05 } else { 0.03 }))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().px_2().py(px(1.)).rounded(px(5.)).bg(color.opacity(if dark { 0.20 } else { 0.12 })).text_color(color).text_xs().font_weight(FontWeight::SEMIBOLD).child(f.verdict.label()))
                            .child(div().text_xs().text_color(p.muted).child(confidence_label(&f.confidence)))
                            .child(div().flex_1())
                            .child(Self::locate_button(("fact-locate", i), f.block_index, cx)),
                    )
                    .child(div().font_family(p.serif.clone()).text_base().line_height(relative(1.7)).text_color(p.fg).child(format!("“{}”", f.quote)))
                    .child(div().text_sm().line_height(relative(1.6)).text_color(p.muted).child(f.explanation.clone())),
            );
        }
        col.into_any_element()
    }

    /// Counts refer to the original in every mode; progress refers to the shown
    /// body's logical rows. Unmeasured virtual rows have zero pixel height, so
    /// the scrollbar's measured pixel total cannot be used as the whole length.
    fn stats_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.insights.view != View::Text { return None; }
        let stats = self.model.stats()?;
        let p = self.model.stats_prefs();
        let theme = cx.theme().clone();
        let has_body = !self.model.layout().rows.is_empty();
        let pct = self.reading_pct;
        let progress = if !has_body {
            stats_label("正文未显示", "Body not shown").to_string()
        } else {
            format!("{} {pct}%", if stats.complete() { stats_label("读到", "Read to") } else { stats_label("已载入部分读到", "Loaded text: read to") })
        };
        let reader = cx.entity().downgrade();
        let list = self.list.clone();
        let probe = canvas(|_, _, _| (), move |_, _, _, cx| {
            // After layout, including resize/font/mode changes and citation jumps.
            if !has_body || list.viewport_bounds().size.height <= px(0.) { return; }
            let current = list_reading_percent(&list);
            if current != pct {
                let reader = reader.clone();
                cx.defer(move |cx| {
                    reader.update(cx, |r, cx| {
                        if r.reading_pct != current { r.reading_pct = current; cx.notify(); }
                    }).ok();
                });
            }
        }).absolute().top_0().left_0().size_full();
        let strip = v_flex().relative().flex_none().pt_2().pb_2().gap_1().border_t_1().border_color(theme.border)
            .child(h_flex().gap_3().flex_wrap().text_xs().text_color(theme.muted_foreground)
                .child(stats.count_label(p))
                .child(stats.time_label(p))
                .child(progress)
                .child(h_flex().gap_1().child(stats.loaded_label()).when_some(stats.loaded_fraction(), |d, f| {
                    d.child(div().w(px(36.)).h(px(3.)).rounded_full().bg(theme.muted).child(div().h_full().w(relative(f)).bg(theme.muted_foreground)))
                }))
                // Right-aligned: the folded conversation's AI button sits over the strip's centre.
                .when_some(self.pass_status(cx), |d, s| d.child(div().ml_auto().child(s))))
            .when(has_body, |d| d.child(div().h(px(2.)).w_full().bg(theme.muted).child(div().h_full().w(relative(pct as f32 / 100.)).bg(theme.primary))))
            .child(probe);
        Some(div().id("reading-statistics").flex_none().tooltip(|window, cx| {
            Tooltip::new(stats_label("按原文统计；读到位置按当前视图的段落位置估算。统计方式和速度可在设置 › 外观更改。", "Counts use the original; reading position is estimated by paragraph position in the current view. Change counting rules and speeds in Settings › Appearance.")).build(window, cx)
        }).child(strip).into_any_element())
    }

    fn status_chip(&self, cx: &App) -> Option<Div> {
        // A book with chapter translation shows that state in its chapter banner;
        // the whole-item translation status would contradict it.
        if self.model.book().is_some() && self.chapter_tr.supported { return None; }
        // In 对照 / 译文 the 翻译 button says the same; a problem keeps its reason here.
        if self.model.mode() != Mode::Original && !self.translate.is_problem() { return None; }
        let theme = cx.theme();
        let text = self.translate.status()?;
        let problem = self.translate.is_problem();
        Some(
            div()
                .px_2()
                .py(px(1.))
                .rounded(px(6.))
                .text_xs()
                .when(problem, |d| d.bg(theme.warning.opacity(0.14)).text_color(theme.foreground))
                .when(!problem, |d| d.bg(theme.secondary).text_color(theme.muted_foreground))
                .child(text),
        )
    }
}

/// What a click on a stretch of block text does.
#[derive(Clone)]
enum Click {
    /// An AI highlight's label (also shown on hover).
    Label(String),
    /// An http/https link, opened in the browser.
    Url(String),
}

#[derive(Default)]
struct SelectionBinding {
    source: String,
    revision: String,
    translation: bool,
    ai: bool,
    /// Display-only speaker name at the start of a translated paragraph:
    /// (its range in the block's shown text, the name shown instead).
    relabel: Option<(Range<usize>, String)>,
    selected: crate::selection::Selection,
}

/// `range` of a block's shown text with its bold runs, code spans, links and
/// AI highlight spans (never italic). Labelled spans show their label on hover
/// and on click; links show their target on hover and open it on click.
fn rich(id: String, b: &Block, range: Range<usize>, spans: &[Span], p: &Palette, reader: &WeakEntity<Reader>, binding: &SelectionBinding) -> AnyElement {
    let clip = |r: &Range<usize>| {
        let (s, e) = (r.start.max(range.start), r.end.min(range.end));
        (s < e).then(|| s - range.start..e - range.start)
    };
    let mut bold: Vec<Range<usize>> = b.bold.iter().filter_map(clip).collect();
    let mut code: Vec<Range<usize>> = b.marks.code.iter().filter_map(clip).collect();
    let mut links: Vec<(Range<usize>, String)> = b.marks.links.iter().filter_map(|(r, u)| clip(r).map(|r| (r, u.clone()))).collect();
    let mut spans: Vec<Span> = spans.iter().filter_map(|s| clip(&s.range).map(|range| Span { range, ..s.clone() })).collect();
    // `<font color=red>` runs are drawn in the rose-red of the palette.
    spans.extend(b.marks.red.iter().filter_map(clip).map(|range| Span { range, style: memex_client::insights::HighlightStyle::Negative, label: None }));
    let mut sub = b.text[range.clone()].to_string();
    // The speaker label shown under another name (display only): every mark moves with it.
    let mut display_label = None;
    if let Some((at, name)) = binding.relabel.as_ref().filter(|(at, _)| range.start <= at.start && at.end <= range.end) {
        let at = at.start - range.start..at.end - range.start;
        let moved = |p: usize| crate::selection::shown_to_display(p, &at, name.len());
        for r in bold.iter_mut().chain(code.iter_mut()).chain(links.iter_mut().map(|(r, _)| r)).chain(spans.iter_mut().map(|s| &mut s.range)) {
            *r = moved(r.start)..moved(r.end);
        }
        sub.replace_range(at.clone(), name);
        display_label = Some((at.start..at.start + name.len(), at.len()));
    }
    // 简繁 is display only: the converted text keeps its character count, so every
    // mark moves to the same characters; selection (which needs source offsets) is off.
    let converted = p.script != memex_client::opencc::Script::Original && b.kind != BlockKind::Code;
    if converted {
        let mut all: Vec<Range<usize>> = bold.iter().chain(&code).chain(links.iter().map(|(r, _)| r)).chain(spans.iter().map(|s| &s.range)).cloned().collect();
        let (text, mapped) = memex_client::opencc::convert_with_ranges(&sub, p.script, &all);
        sub = text;
        all = mapped;
        let mut it = all.into_iter();
        for r in bold.iter_mut().chain(code.iter_mut()).chain(links.iter_mut().map(|(r, _)| r)).chain(spans.iter_mut().map(|s| &mut s.range)) {
            if let Some(m) = it.next() { *r = m; }
        }
    }
    let link_ranges: Vec<Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
    let runs: Vec<(Range<usize>, HighlightStyle)> = merge(sub.len(), &bold, &code, &link_ranges, &spans)
        .into_iter()
        .map(|(r, m)| {
            let mut h = HighlightStyle::default();
            if m.bold {
                h.font_weight = Some(FontWeight::BOLD);
            }
            if m.code {
                h.background_color = Some(p.code_bg);
            }
            if m.link {
                h.color = Some(p.rose);
                h.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(p.rose.opacity(0.6)), wavy: false });
            }
            if m.highlight {
                h.background_color = Some(p.hl_bg);
            }
            if m.positive {
                h.color = Some(p.pos);
            }
            if m.negative {
                h.color = Some(p.neg);
            }
            if m.underline {
                h.underline = Some(UnderlineStyle { thickness: px(1.5), color: Some(p.pos), wavy: false });
            }
            (r, h)
        })
        .collect();
    let styled = StyledText::new(sub.clone()).with_highlights(runs);
    let text_layout = styled.layout().clone();
    let mut clicks: Vec<(Range<usize>, Click)> = links.into_iter().map(|(r, u)| (r, Click::Url(u))).collect();
    clicks.extend(spans.iter().filter_map(|s| s.label.clone().map(|l| (s.range.clone(), Click::Label(l)))));
    let tips = clicks.clone();
    let reader = reader.clone();
    let interactive = InteractiveText::new(ElementId::Name(format!("{id}-links").into()), styled)
        .tooltip(move |ix, window, cx| {
            tips.iter().find(|(r, _)| r.contains(&ix)).map(|(_, c)| {
                let text = match c {
                    Click::Label(l) => l.clone(),
                    Click::Url(u) => u.clone(),
                };
                Tooltip::new(text).build(window, cx)
            })
        })
        .on_click(clicks.iter().map(|(r, _)| r.clone()).collect(), move |i, window, cx| {
            if gpui_kit::base::TextSelection::has_selection(window, cx) { return; }
            match clicks.get(i).map(|(_, c)| c.clone()) {
            Some(Click::Url(url)) => {
                // Checked again here: only http/https ever reaches the browser.
                if let Some(url) = memex_client::markdown::safe_url(&url) {
                    cx.open_url(url);
                }
            }
            Some(Click::Label(label)) => {
                reader
                    .update(cx, |r, cx| {
                        r.picked = Some(label);
                        cx.notify();
                    })
                    .ok();
            }
            None => {}
        }});
    if converted { return interactive.into_any_element(); }
    crate::selection::SelectableRich { id: format!("{}-{id}", binding.revision), text: sub.into(), interactive, layout: text_layout,
        source: binding.source.clone(), block_start: b.start, skip: b.skip, shown_start: range.start,
        code: b.kind == BlockKind::Code, translation: binding.translation, ai: binding.ai, relabel: display_label,
        order: ((b.start + range.start) as u64) * 2 + u64::from(binding.translation), selected: binding.selected.clone() }.into_any_element()
}

/// The lines of `range` laid out: list items with their markers and depth,
/// quotes with a rule, horizontal rules, table rows as cells.
fn lines_element(b: &Block, range: Range<usize>, text: &dyn Fn(Range<usize>, String) -> AnyElement, p: &Palette) -> Div {
    let mut col = v_flex().gap_1();
    let mut row_ix = 0;
    for (n, line) in b.marks.lines.iter().enumerate().filter(|(_, l)| l.range.start >= range.start && l.range.end <= range.end) {
        let body = |r: Range<usize>| text(r, format!("l{n}"));
        col = col.child(match &line.kind {
            LineKind::Text => div().child(body(line.range.clone())),
            LineKind::Item { depth, marker } => {
                let bullet = marker.clone().unwrap_or_else(|| ["•", "◦", "▪"][(*depth as usize).min(2)].to_string());
                h_flex()
                    .items_start()
                    .pl(px(20. * *depth as f32))
                    .child(div().flex_none().min_w(px(20.)).pr_1().text_color(p.muted).child(bullet))
                    .child(div().flex_1().min_w_0().child(body(line.range.clone())))
            }
            LineKind::Quote { depth } => div().pl(px(12. * (*depth as f32 - 1.).max(0.))).child(div().pl_3().border_l_2().border_color(p.rose.opacity(0.45)).text_color(p.warm).child(body(line.range.clone()))),
            LineKind::Rule => div().my_2().h(px(1.)).w_full().bg(p.muted.opacity(0.35)),
            LineKind::Skip => div(),
            LineKind::Row { cells, header } => {
                row_ix += 1;
                let mut row = h_flex().items_start().border_b_1().border_color(p.muted.opacity(0.3)).when(*header, |d| d.bg(p.code_bg)).when(row_ix == 1, |d| d.border_t_1());
                for (k, c) in cells.iter().enumerate() {
                    row = row.child(div().flex_1().min_w_0().px_2().py_1().text_sm().when(*header, |d| d.font_weight(FontWeight::SEMIBOLD)).child(text(c.clone(), format!("l{n}c{k}"))));
                }
                row
            }
        });
    }
    col
}

/// One block. `under` marks a translation shown beneath its original (对照);
/// `spans` are the block's AI highlights (original text only).
/// Whether the block before `key` (same side) is not a note: the note run starts here.
fn note_run_starts(key: &str, reader: &WeakEntity<Reader>, cx: &App) -> bool {
    let translation = key.starts_with('t');
    let Some(i) = key.get(1..).and_then(|n| n.parse::<usize>().ok()) else { return true };
    if i == 0 { return true; }
    reader.read_with(cx, |r, _| {
        let side = if translation { r.model.translation() } else { r.model.original() };
        side.get(i - 1).is_none_or(|prev| !memex_client::is_note_text(&prev.text))
    }).unwrap_or(true)
}

/// The end of a book chapter: 下一章 · title, or 全书完 after the last one.
/// One 段落 row: the original blocks (smaller, warm) above their translation
/// blocks. `t_prefix` keys the translation side: `t` the human edition, `a`
/// an AI translation (selection binds each to its own text).
fn aligned_row(r: &crate::model::AlignedRow, original: &[Block], translation: &[Block], t_prefix: char, t_spans: &HashMap<usize, Vec<Span>>, p: &Palette, reader: &WeakEntity<Reader>, cx: &mut App) -> AnyElement {
    use memex_client::book::{PairKind, Relation};
    let mut secondary = p.clone();
    secondary.fg = p.warm;
    secondary.body_size = p.body_size - 3.;
    let mut primary = p.clone();
    primary.muted = p.fg;
    primary.warm = p.fg;
    let note = r.kind == PairKind::Note;
    if note {
        secondary.body_size -= 1.;
        primary.body_size -= 3.;
        primary.fg = p.muted;
    }
    let both = r.relation == Relation::Matched;
    let tag = match r.relation {
        Relation::Matched => None,
        Relation::OriginalOnly => Some(tx("仅原文")),
        Relation::TranslationOnly => Some(tx("仅译文")),
        Relation::Unresolved => Some(tx("未匹配")),
    };
    let mut col = v_flex().pb_1().when(note, |d| d.pl_4().border_l_2().border_color(p.muted.opacity(0.3)));
    if note || tag.is_some() {
        col = col.child(h_flex().gap_2().pt_1().text_xs().text_color(p.muted)
            .when(note, |d| d.child(tx("注")))
            .when_some(tag, |d, t| d.child(div().px_1().rounded(px(3.)).border_1().border_color(p.muted.opacity(0.4)).child(t))));
    }
    for i in r.o.clone() {
        if let Some(b) = original.get(i) {
            col = col.child(block_element(b, if both { &secondary } else { p }, false, false, &[], format!("o{i}"), reader, cx));
        }
    }
    for i in r.t.clone() {
        if let Some(b) = translation.get(i) {
            col = col.child(block_element(b, &primary, both, false, t_spans.get(&i).map(Vec::as_slice).unwrap_or(&[]), format!("{t_prefix}{i}"), reader, cx));
        }
    }
    col.into_any_element()
}

fn chapter_end(next: Option<(usize, String)>, p: &Palette, reader: &WeakEntity<Reader>, id: &'static str) -> AnyElement {
    let reader = reader.clone();
    let foot = h_flex().max_w(px(720.)).justify_center().mt_6().mb_8().pt_6().border_t_1().border_color(p.muted.opacity(0.25));
    match next {
        Some((at, title)) => foot.child(Button::new(id).outline().label(format!("{} ›", memex_client::txf("下一章 · {0}", &[&title])))
            .on_click(move |_, _, cx| { reader.update(cx, |r, cx| r.go_chapter(at, cx)).ok(); })).into_any_element(),
        None => foot.child(div().text_sm().text_color(p.muted).child(tx("全书完"))).into_any_element(),
    }
}

fn block_element(b: &Block, p: &Palette, under: bool, lit: bool, spans: &[Span], key: String, reader: &WeakEntity<Reader>, cx: &mut App) -> Div {
    let binding = reader.read_with(cx, |r, _| {
        let Some(detail) = r.model.detail() else { return SelectionBinding::default() };
        // `a…`: the AI translation of an original-only chapter (never the human edition).
        let ai = key.starts_with('a');
        let translation = ai || key.starts_with('t');
        let markdown = if ai { r.tr_markdown() } else { r.model.side_markdown(translation) };
        let relabel = ((key.starts_with('t') || ai) && b.kind == BlockKind::Paragraph)
            .then(|| r.translated_speakers().relabel_t(&b.text, memex_client::speaker_names())).flatten();
        SelectionBinding { source: markdown.get(b.start..b.end).unwrap_or("").into(), revision: detail.canonical.revision_id.clone(), translation, ai, relabel, selected: r.selected.clone() }
    }).unwrap_or_default();
    // Called by the virtual list only for laid-out rows. Keep a small LRU of
    // revision-scoped entities, never fetch every picture in a long document.
    let images = reader.update(cx, |r, cx| {
        let mut images = HashMap::new();
        let Some(detail) = r.model.detail() else { return images };
        // Book chapters carry no fetchable revision images.
        if r.model.book().is_some() { return images; }
        let (item, revision) = (detail.canonical.item_id.clone(), detail.canonical.revision_id.clone());
        for target in b.href.iter().chain(b.marks.images.iter().map(|(_, t)| t)) {
            if memex_client::images::local_image_target(target).is_none() { continue }
            let cached = r.images.iter().position(|(t, _)| t == target).and_then(|i| r.images.remove(i));
            let entity = cached.map(|(_, e)| e).unwrap_or_else(|| {
                cx.new(|cx| crate::image::ReaderImage::new(r.client.clone(), item.clone(), revision.clone(), target.clone(), cx))
            });
            r.images.push_back((target.clone(), entity.clone()));
            while r.images.len() > 16 { r.images.pop_front(); }
            images.insert(target.clone(), entity);
        }
        images
    }).unwrap_or_default();
    let base = div().max_w(px(p.measure)).font_family(p.serif.clone()).when(lit, |d| d.bg(p.accent.opacity(0.12)).rounded(px(4.)).px_1());
    let base = if under {
        base.mt(px(2.)).mb_2().text_color(p.fg)
    } else {
        base.py_2().text_color(p.fg)
    };
    let len = b.text.len();
    let text = |range: Range<usize>, part: String| {
        let mut from = range.start;
        let mut col = v_flex();
        let mut found = false;
        for (shown, target) in &b.marks.images {
            if shown.start < range.start || shown.end > range.end { continue }
            let Some(image) = images.get(target) else { continue };
            if from < shown.start { col = col.child(rich(format!("{key}-{part}-{from}"), b, from..shown.start, spans, p, reader, &binding)); }
            // A local poster may be nested in an ordinary safe Markdown link.
            // Rendering its image must not swallow the original video destination.
            let link = b.marks.links.iter().find(|(r, _)| r.start <= shown.start && r.end >= shown.end)
                .and_then(|(_, url)| memex_client::markdown::safe_url(url)).map(str::to_owned);
            col = if let Some(url) = link {
                col.child(div().id(SharedString::from(format!("{key}-{part}-poster-{}", shown.start)))
                    .cursor_pointer().child(image.clone())
                    .tooltip({ let url = url.clone(); move |window, cx| Tooltip::new(url.clone()).build(window, cx) })
                    .on_click(move |_, window, cx| {
                        if !gpui_kit::base::TextSelection::has_selection(window, cx) { cx.open_url(&url); }
                    }))
            } else { col.child(image.clone()) };
            from = shown.end;
            found = true;
        }
        if !found { return rich(format!("{key}-{part}"), b, range, spans, p, reader, &binding) }
        if from < range.end { col = col.child(rich(format!("{key}-{part}-{from}"), b, from..range.end, spans, p, reader, &binding)); }
        col.into_any_element()
    };
    let paragraph = |d: Div| d.text_size(px(p.body_size)).line_height(relative(1.8));
    // Text, or its lines laid out when the block has lists, quotes, rules or tables.
    let body = |range: Range<usize>, part: &str| -> AnyElement {
        if b.marks.lines.is_empty() {
            text(range, part.to_string())
        } else {
            lines_element(b, range, &|r, k| text(r, format!("{part}{k}")), p).into_any_element()
        }
    };
    match b.kind {
        BlockKind::Heading(level) => {
            // The agent counts text right under a heading line as part of its block.
            let cut = b.heading_len.min(len);
            let head_end = b.text[..cut].trim_end().len();
            let rest_start = len - b.text[cut..].trim_start_matches('\n').len();
            let head = div().child(text(0..head_end, "h".into()));
            let head = match level {
                1 => head.text_size(px(p.body_size + 6.)).font_weight(FontWeight::SEMIBOLD),
                _ => head.text_size(px(p.body_size + 2.)).font_weight(FontWeight::SEMIBOLD),
            };
            let rest = (rest_start < len).then(|| paragraph(div().mt_2().font_weight(FontWeight::NORMAL)).child(body(rest_start..len, "p")));
            base.when(level > 1 && !under, |d| d.pt_3()).child(head).children(rest)
        }
        // 注释: one size smaller, muted, in the note typeface; a rule above the first of a run.
        BlockKind::Paragraph if memex_client::is_note_text(&b.text) => {
            let first = note_run_starts(&key, reader, cx);
            base.font_family(memex_client::note_font().family()).text_color(p.fg.opacity(0.72)).text_size(px(p.body_size - 2.)).line_height(relative(1.7))
                .when(first, |d| d.mt_3().pt_3().border_t_1().border_color(p.muted.opacity(0.3)))
                .child(body(0..len, "p"))
        }
        BlockKind::Paragraph => paragraph(base).child(body(0..len, "p")),
        BlockKind::Image => {
            if let Some(image) = b.href.as_ref().and_then(|t| images.get(t)) {
                return base.child(image.clone());
            }
            let label = image_placeholder(&b.text);
            let chip = base.my_1().px_3().py_2().rounded(px(6.)).border_1().border_color(p.muted.opacity(0.45)).text_sm().text_color(p.muted);
            match b.href.clone() {
                // A labelled link to the image; the reader itself never loads it.
                Some(url) => chip.child(
                    h_flex()
                        .id(SharedString::from(format!("{key}-img")))
                        .gap_1()
                        .cursor_pointer()
                        .text_color(p.rose)
                        .child(memex_client::markdown::image_label(&b.text))
                        .child(Icon::new(IconName::ExternalLink).xsmall())
                        .tooltip({
                            let url = url.clone();
                            move |window, cx| Tooltip::new(url.clone()).build(window, cx)
                        })
                        .on_click(move |_, _, cx| {
                            if let Some(url) = memex_client::markdown::safe_url(&url) {
                                cx.open_url(url);
                            }
                        }),
                ),
                None => chip.child(label),
            }
        }
        BlockKind::Code => {
            let start = b.text.find('\n').map_or(0, |i| i + 1);
            let end = b.text.rfind('\n').filter(|&i| b.text[i..].trim() == "```" || b.text[i..].trim() == "~~~").unwrap_or(b.text.len());
            base.my_1().px_3().py_2().rounded(px(6.)).bg(p.code_bg).font_family("Menlo").text_xs().line_height(relative(1.6)).child(text(start..end.max(start), "code".into()))
        },
    }
}

impl Render for Reader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Set again while painting, if the button is still shown.
        self.minutes_button.set(None);
        let theme = cx.theme().clone();
        let style = self.item_style();
        let serif = SharedString::from(self.item_font().family());
        let mut tabs = h_flex().gap_1().px_2().pt_1().border_b_1().border_color(theme.border).overflow_x_hidden();
        for (i, tab) in self.model.tabs().iter().enumerate() {
            let active = self.model.active() == Some(i);
            tabs = tabs.child(
                h_flex()
                    .id(("tab", i))
                    .gap_1()
                    .px_3()
                    .py_1()
                    .max_w(px(240.))
                    .rounded_t(px(6.))
                    .text_xs()
                    .when(active, |d| d.bg(theme.background).border_1().border_b_0().border_color(theme.border))
                    .when(!active, |d| d.text_color(theme.muted_foreground).hover(|d| d.bg(theme.secondary)))
                    // CJK titles have no italic: a small 预览 tag marks the preview tab.
                    .when(!tab.pinned, |d| d.child(div().flex_none().px_1().rounded(px(3.)).border_1().border_color(theme.border).text_color(theme.muted_foreground).child(tx("预览"))))
                    .child(div().truncate().when(!tab.pinned, |d| d.italic()).child(tab.title.clone()))
                    .when(!tab.pinned, |d| d.tooltip(|window, cx| Tooltip::new(tx("预览标签页：双击固定")).build(window, cx)))
                    .child(
                        div()
                            .id(("tab-close", i))
                            .child(Icon::new(IconName::Close).xsmall())
                            .on_click(cx.listener(move |this, _, _, cx| this.close(i, cx))),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() >= 2 { this.pin_tab(i, cx); } else { this.activate(i, cx); }
                    })),
            );
        }

        let body = if self.fmt.batch.is_some() {
            // 批量格式整理 replaces the text; 查看修改 of a row replaces the list.
            let inner = match &self.compare {
                Some((compare, _)) => v_flex().flex_1().min_h_0().child(compare.clone()).children(self.format_review_panel(cx)).into_any_element(),
                None => v_flex().flex_1().min_h_0().children(self.format_batch_panel(cx)).children(self.format_review_panel(cx)).into_any_element(),
            };
            v_flex().flex_1().min_h_0().overflow_hidden().px_6().pt_4().child(inner).into_any_element()
        } else if let Some(detail) = self.model.detail().cloned() {
            let mode = self.model.mode();
            let layout = self.model.layout();
            let original = self.model.original().to_vec();
            let translation = self.model.translation().to_vec();
            let title = detail.canonical.title.clone();
            // 中译标题 as soon as the title step succeeded, even if the document failed.
            let title_tr = detail.analysis.chinese_title(&title);
            let highlight = self.model.highlight();
            let mut palette = Palette::new(&theme);
            palette.body_size = self.reading_size.pixels();
            palette.serif = serif.clone();
            palette.script = style.script;
            // 简繁 / 竖排 are offered for Chinese text (and kept once chosen).
            let chinese = !style.is_default() || looks_chinese(&title, &original.iter().take(40).map(|b| b.text.as_str()).collect::<String>());
            let view = self.insights.view;
            // 重点标注 on the side being read: the original (原文, its half of 对照)
            // and, when the agent marked it, the translation (译文, its half of 对照).
            // The 重点 switch decides which layers are drawn (AI / manual).
            let (show_ai, show_mine) = self.lr_shows();
            let emphasis = show_ai && self.insights.highlights().is_some();
            let in_book = self.model.book().is_some();
            // Highlights and underlines address the canonical text, not a book chapter.
            let items = self.insights.highlights().filter(|_| emphasis && !in_book);
            let (side_o, side_t) = marked_sides(layout.shown, self.insights.translation_marked());
            let (mut spans, skipped) = match items.filter(|_| side_o) {
                Some(items) => block_spans(&detail.canonical.markdown, &original, items, Side::Original),
                None => (HashMap::new(), 0),
            };
            if show_mine && !in_book && side_o {
                for (i, v) in self.manual_spans(&detail.canonical.markdown, &original) { spans.entry(i).or_default().extend(v); }
            }
            if let Some(annotations) = self.annotations.as_ref().filter(|a| show_mine && !a.underlines.is_empty() && !in_book) {
                for (index, block) in original.iter().enumerate() {
                    let raw = &detail.canonical.markdown[block.start..block.end];
                    let inline = crate::inline::Inline::parse(raw, block.skip);
                    for mark in &annotations.underlines {
                        let start = mark.start.max(block.start);
                        let end = mark.end.min(block.end);
                        if start >= end || block.kind == BlockKind::Image { continue; }
                        let shown = if block.kind == BlockKind::Code { Some(start - block.start..end - block.start) } else { inline.shown(start - block.start..end - block.start) };
                        if let Some(range) = shown.filter(|r| r.start < r.end) {
                            spans.entry(index).or_default().push(Span { range, style: memex_client::insights::HighlightStyle::Other("manual_underline".into()), label: Some("手工划线".into()) });
                        }
                    }
                }
            }
            let (spans_t, skipped_t) = match (items.filter(|_| side_t), detail.analysis.markdown.as_deref()) {
                (Some(items), Some(md)) => block_spans(md, &translation, items, Side::Translation),
                _ => (HashMap::new(), 0),
            };
            self.skipped_highlights = skipped + skipped_t;
            self.lr.terms = Rc::new(self.lr_terms(&layout.rows, &spans, &spans_t, &palette));
            if !in_book { self.lr_track_position(&layout.rows, cx); }
            let spans = Rc::new(spans);
            let spans_t = Rc::new(spans_t);
            let reader = cx.entity().downgrade();
            // As the list and the phone: the translated title leads, then 「｜」 and the original.
            let (head_title, sub_title) = match memex_client::title_parts(&title, title_tr.as_deref()) {
                (Some(t), original) => (t.to_string(), Some(original.to_string())),
                (None, original) => (original.to_string(), None),
            };
            // 简繁 shows the title converted too; 点击复制 still copies the stored title.
            let shown_title = memex_client::opencc::convert(&head_title, style.script);
            let dates = self.dates_line(&detail.canonical.item_id).filter(|_| !self.compact);
            let mut switch = h_flex().rounded(px(7.)).bg(theme.muted).p(px(2.));
            for (m, id, label) in [(Mode::Original, "mode-original", tx("原文")), (Mode::Bilingual, "mode-bilingual", tx("对照")), (Mode::Translation, "mode-translation", tx("译文"))] {
                switch = switch.child(
                    Button::new(id)
                        .small()
                        .label(label)
                        .when(mode == m, |b| b.primary())
                        .when(mode != m, |b| b.ghost())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_mode(m, cx))),
                );
            }
            // In 原文 the action sits in the header; 对照/译文 show it in their empty state.
            // A source with nothing to translate (Chinese) offers its minutes in the header.
            let minutes_only = self.translate.not_needed() || !layout.missing_translation;
            let header_offer = if mode == Mode::Original || minutes_only { self.offer("translate-header", cx) } else { None };
            // The card is the list's first row when the article is shown, so it
            // scrolls with the text; the header keeps only title and toolbar.
            let m = self.translate.minutes();
            let key = Some((m.map(|m| m.state.clone()), m.and_then(|m| m.text()).map_or(0, str::len), self.minutes_collapsed, self.minutes_full, self.translate.minutes_ask().clone(), self.minutes_hidden));
            if key != self.card_key {
                self.card_key = key;
                self.list.remeasure_items(0..LEAD);
            }
            let header = v_flex()
                .flex_none()
                .gap_2()
                .pb_3()
                .when(self.compact, |d| d.gap_1().pb_2())
                .child(h_flex().items_start().gap_2().child(div().id("reader-copy-title").cursor_pointer().flex_1().min_w_0()
                    .tooltip(|window, cx| Tooltip::new(tx("点击复制标题")).build(window, cx))
                    .on_click({ let title = head_title.clone(); move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(title.clone())) })
                    .font_family(serif.clone()).when(self.compact, |d| d.text_lg().truncate()).when(!self.compact, |d| d.text_2xl()).font_weight(FontWeight::SEMIBOLD).child(shown_title))
                    .child(Button::new("markdown-mode").small().outline().label(tx(if self.editing.visible { "阅读" } else { "编辑 Markdown" }))
                        .disabled(!self.edit_button_enabled()).when_some(self.edit_button_hint(), |b, hint| b.tooltip(tx(hint)))
                        .on_click(cx.listener(|r, _, window, cx| r.toggle_editor(window, cx)))))
                .when_some(sub_title.filter(|_| !self.compact), |d, t| {
                    d.child(
                        h_flex()
                            .mt(px(-4.))
                            .gap_2()
                            .items_baseline()
                            .font_family(serif.clone())
                            .text_lg()
                            .text_color(theme.muted_foreground)
                            .child(div().flex_none().text_color(palette.warm.opacity(0.7)).child("｜"))
                            .child(div().id("reader-copy-original-title").cursor_pointer().min_w_0()
                                .tooltip(|window, cx| Tooltip::new(tx("点击复制标题")).build(window, cx))
                                .on_click({ let title = t.clone(); move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(title.clone())) }).child(t)),
                    )
                })
                .when_some(dates, |d, line| d.child(div().id("reader-dates").text_sm().text_color(theme.muted_foreground).child(line)))
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(self.lr_lead_buttons(cx))
                        .child(switch)
                        .child(self.lr_filter_button(cx))
                        .when_some(self.insight_buttons(cx), |d, b| d.child(b))
                        .when_some(self.format_button(cx), |d, b| d.child(b))
                        .when_some(self.translate_button(cx), |d, b| d.child(b))
                        .when(chinese, |d| d.child(self.guji_controls(cx)))
                        .child(self.font_button(cx))
                        .child(self.size_button(cx))
                        .when_some(self.status_chip(cx), |d, c| d.child(c))
                        .when_some(header_offer, |d, o| d.child(o)),
                )
                .when_some(self.book_bar(cx), |d, b| d.child(b))
                .when_some(self.pairing_line(cx), |d, b| d.child(b))
                .when_some(self.translation_line(cx), |d, b| d.child(b))
                .when_some(self.format_line(cx), |d, b| d.child(b))
                .when_some(self.format_review_panel(cx).filter(|_| self.compare.is_none()), |d, b| d.child(b))
                .child(self.reading_actions(cx))
                .when_some(self.editing.message.clone().or_else(|| self.annotation_notice.clone()), |d, message| d.child(div().text_xs().text_color(theme.muted_foreground).child(message)))
                .when_some(self.lr.notice.clone(), |d, n| d.child(h_flex().gap_2().p_2().rounded(px(6.)).bg(theme.secondary).text_xs().child(Icon::new(IconName::Info).xsmall()).child(n)))
                .when_some(self.lr.mark_notice.clone(), |d, n| d.child(h_flex().gap_2().text_xs().text_color(theme.muted_foreground).child(n)
                    .when(self.lr.pending.is_some(), |d| d.child(Button::new("mark-retry-same").xsmall().outline().label(tx("重试同一请求"))
                        .on_click(cx.listener(|r, _, _, cx| if let Some(p) = r.lr.pending.clone() { r.lr_send_mark(p, cx) }))))))
                .when_some(self.ai_marks.notice.clone(), |d, n| d.child(h_flex().gap_2().text_xs().text_color(theme.muted_foreground).child(n)
                    .when(self.ai_marks.pending.is_some() && !self.ai_marks.busy, |d| d.child(Button::new("ai-mark-retry-same").xsmall().outline().label(tx("重试同一请求"))
                        .on_click(cx.listener(|r, _, _, cx| if let Some(p) = r.ai_marks.pending.clone() { r.ai_send(p, cx) }))))))
                .when_some(self.emphasis_line(layout.shown, &palette, cx), |d, l| d.child(l))
                .when(layout.mismatch, |d| {
                    d.child(h_flex().gap_2().p_2().rounded(px(6.)).bg(theme.secondary).text_xs().child(Icon::new(IconName::Info).xsmall()).child(tx("译文与原文的段落数不一致，无法逐段对照，现只显示译文")))
                })
                .when_some(self.guji_line(mode, layout.missing_translation, cx), |d, l| d.child(l))
                .when_some(self.busy_line(cx), |d, l| d.child(l))
                .when_some(self.model.notice().map(relabel), |d, n| {
                    d.child(h_flex().gap_2().p_2().rounded(px(6.)).bg(theme.secondary).text_xs().child(Icon::new(IconName::Info).xsmall()).child(n))
                });
            // A Chinese source needs no translation: no "missing" banner, just the status.
            let not_needed = self.translate.not_needed();
            let banner = layout.missing_translation && !not_needed;
            let content = if let Some((compare, _)) = self.compare.as_ref().filter(|_| !self.editing.visible) {
                v_flex().flex_1().min_h_0().child(compare.clone()).children(self.format_review_panel(cx)).into_any_element()
            } else if self.editing.visible {
                self.editor_panel()
            } else if view == View::Minutes {
                self.minutes_panel(&palette, cx)
            } else if view == View::Quotes {
                self.quotes_panel(&palette, cx)
            } else if view == View::FactCheck {
                self.facts_panel(&palette, cx)
            } else if in_book && self.toc_open {
                self.toc_panel(&palette, cx)
            } else if let Some(ai) = self.ai_translation_content(mode, &palette, cx) {
                ai
            } else if let Some(missing) = self.model.book().and_then(|b| {
                let empty = |s: &Option<String>| s.as_deref().is_none_or(str::is_empty);
                match mode {
                    Mode::Original if empty(&b.original) => Some(tx("这一章只有译文，原文版本里没有对应章节")),
                    Mode::Translation if empty(&b.translation) => Some(tx("这一章只有原文，译本里没有对应章节")),
                    _ => None,
                }
            }) {
                v_flex().flex_1().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(missing).into_any_element()
            } else if style.vertical && !(if mode == Mode::Translation { &translation } else { &original }).is_empty() {
                self.vertical_panel(mode == Mode::Translation, &palette, cx)
            } else if self.model.book_columns() {
                self.sync_columns();
                let synced = self.model.aligned().iter().any(|r| r.relation == memex_client::book::Relation::Matched);
                v_flex().flex_1().min_h_0().gap_1()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tx(if synced { "两侧按已匹配段落跟随滚动" } else { "这一章还没有段落对应，两侧暂不跟随滚动" })))
                    .child(self.book_columns(&palette, cx))
                    .into_any_element()
            } else if layout.missing_translation && layout.rows.is_empty() {
                // 译文 without a translation.
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .children(self.minutes_card(serif.clone(), cx))
                    .child(if not_needed { memex_client::translation_reason_label("source_is_chinese") } else { tx("这篇还没有译文").to_string() })
                    .into_any_element()
            } else {
                let rows = layout.rows.clone();
                let end = self.chapter_after();
                let chat_end = (!in_book).then(|| self.lr.chat.clone());
                let aligned = self.model.aligned().to_vec();
                let mut p = palette.clone();
                p.measure = longread::MEASURE;
                let tr_title = title_tr.clone().unwrap_or_default();
                let card_font = serif.clone();
                let lst = list(self.list.clone(), move |ix, _window, cx| {
                    if ix < LEAD {
                        // Laid out after the reader's render returned, so the entity is free.
                        return reader
                            .upgrade()
                            .map(|r| r.update(cx, |r, cx| r.lead_row(card_font.clone(), cx)))
                            .unwrap_or_else(|| div().into_any_element());
                    }
                    let ix = ix - LEAD;
                    let Some(row) = rows.get(ix).copied() else { return div().into_any_element() };
                    let last = ix + 1 == rows.len();
                    // The header already shows the title; skip a leading H1 that repeats it.
                    let repeats = |b: &Block, t: &str| ix == 0 && b.kind == BlockKind::Heading(1) && b.text.trim() == t.trim();
                    let lit = |i: usize| highlight == Some(i);
                    let on = |i: usize| spans.get(&i).map(Vec::as_slice).unwrap_or(&[]);
                    let on_t = |i: usize| spans_t.get(&i).map(Vec::as_slice).unwrap_or(&[]);
                    let el = match row {
                        Row::Original(i) | Row::Shared(i) => match original.get(i) {
                            Some(b) if !repeats(b, &title) => block_element(b, &p, false, lit(i), on(i), format!("o{i}"), &reader, cx).into_any_element(),
                            _ => div().into_any_element(),
                        },
                        Row::Translation(i) => match translation.get(i) {
                            Some(b) if !repeats(b, &tr_title) => block_element(b, &p, false, lit(i), on_t(i), format!("t{i}"), &reader, cx).into_any_element(),
                            _ => div().into_any_element(),
                        },
                        Row::Aligned(k) => match aligned.get(k) {
                            Some(r) => aligned_row(r, &original, &translation, 't', &HashMap::new(), &p, &reader, cx),
                            None => div().into_any_element(),
                        },
                        Row::Pair(i) => match (original.get(i), translation.get(i)) {
                            (Some(o), _) if repeats(o, &title) => div().into_any_element(),
                            (Some(o), Some(t)) => {
                                let mut secondary = p.clone();
                                secondary.fg = p.warm;
                                secondary.body_size = p.body_size - 3.;
                                let mut primary = p.clone();
                                primary.muted = p.fg;
                                primary.warm = p.fg;
                                v_flex().pb_1()
                                    .child(block_element(o, &secondary, false, lit(i), on(i), format!("o{i}"), &reader, cx))
                                    .child(block_element(t, &primary, true, false, on_t(i), format!("t{i}"), &reader, cx))
                                    .into_any_element()
                            },
                            _ => div().into_any_element(),
                        },
                    };
                    match (&end, last) {
                        (Some(next), true) => v_flex().child(el).child(chapter_end(next.clone(), &p, &reader, "chapter-end")).into_any_element(),
                        (None, true) if chat_end.is_some() => {
                            // Laid out after the reader's render returned, so the entity is free.
                            let card = reader.upgrade().and_then(|r| r.update(cx, |r, cx| r.pass_card(&p, cx)));
                            v_flex().child(el).child(Self::lr_end_zone(chat_end.as_deref().unwrap(), &p, card)).into_any_element()
                        }
                        _ => el,
                    }
                })
                .flex_1();
                let overlays = if view == View::Text { self.lr_overlays(&layout.rows, &palette, cx) } else { vec![] };
                let docked = (self.lr.toc_open && self.lr.toc_docked && !in_book).then(|| self.lr_toc(true, &layout.rows, cx));
                // The rail and knobs keep to the text's right edge, however wide the pane.
                let lst = div().relative().flex_1().min_h_0().max_w(px(longread::MEASURE + longread::GUTTER)).flex().flex_col().pr(px(longread::GUTTER)).child(lst).children(overlays);
                let lst = h_flex().flex_1().min_h_0().items_stretch().children(docked).child(lst);
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .when(banner, |d| {
                        // 对照 without a translation: the original stays readable.
                        d.child(
                            h_flex()
                                .gap_2()
                                .mb_2()
                                .px_3()
                                .py_2()
                                .max_w(px(720.))
                                .rounded(px(6.))
                                .bg(theme.accent.opacity(0.6))
                                .text_sm()
                                .child(Icon::new(IconName::Info).small().text_color(theme.primary))
                                .child(div().flex_1().child(tx("还没有译文，暂时只显示原文"))),
                        )
                    })
                    .child(lst)
                    .into_any_element()
            };
            // Clipped: a pane given less height than its content never paints
            // outside its own bounds (into the conversation below it).
            v_flex().flex_1().min_h_0().overflow_hidden().px_6().when(self.compact, |d| d.pt_2()).when(!self.compact, |d| d.pt_4()).child(header).child(content).children(self.stats_strip(cx)).into_any_element()
        } else {
            let message = if self.loading {
                tx("正在载入…").to_string()
            } else {
                self.error.as_deref().map(relabel).unwrap_or_else(|| tx("从右侧条目中选择一篇开始阅读").into())
            };
            v_flex().flex_1().items_center().justify_center().gap_2().text_sm().text_color(theme.muted_foreground).child(message)
                .when(!self.loading && self.busy_retry.is_some(), |d| d.child(Button::new("reader-busy-retry-item").small().outline().label(tx("重试"))
                    .on_click(cx.listener(|r, _, _, cx| r.retry_busy(cx)))))
                .into_any_element()
        };

        v_flex().relative().size_full().min_h_0().overflow_hidden().bg(theme.background)
            .track_focus(&self.focus).key_context(longread::READER_CONTEXT)
            .on_mouse_down(MouseButton::Left, cx.listener(|r, _, window, cx| { r.focus.focus(window, cx); r.pass_activity(cx); }))
            .on_scroll(cx.listener(|r, _, _, cx| r.pass_activity(cx)))
            .on_key_down(cx.listener(|r, _, _, cx| r.pass_activity(cx)))
            .on_action(cx.listener(|r, _: &longread::MarkBold, window, cx| r.on_mark_key(memex_client::reader_state::MarkStyle::Bold, window, cx)))
            .on_action(cx.listener(|r, _: &longread::MarkRed, window, cx| r.on_mark_key(memex_client::reader_state::MarkStyle::Red, window, cx)))
            .on_action(cx.listener(|r, _: &longread::MarkBlue, window, cx| r.on_mark_key(memex_client::reader_state::MarkStyle::Blue, window, cx)))
            .on_action(cx.listener(|r, _: &longread::MarkUnderline, window, cx| r.on_mark_key(memex_client::reader_state::MarkStyle::Underline, window, cx)))
            .on_action(cx.listener(|r, _: &longread::MarkYellow, window, cx| r.on_mark_key(memex_client::reader_state::MarkStyle::Yellow, window, cx)))
            .when(!self.model.tabs().is_empty(), |d| d.child(tabs.flex_none())).child(body)
            .children(self.lr_filter_menu(cx))
            .children(self.lr_selection_bar(cx))
            .children(self.pass_stats_panel(cx))
    }
}

/// The reader's book view of an item: `None` when it is not a paired book or the
/// agent has no `reader_v1`.
type OpenedBook = Option<(crate::model::Book, Option<String>, Option<String>, crate::model::Paired)>;

fn open_book(client: &Client, item_id: &str, revision_id: &str) -> Result<OpenedBook, memex_client::ClientError> {
    let doc = match client.reader_toc(item_id, revision_id) {
        Ok(doc) => doc,
        Err(memex_client::ClientError::Unsupported) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(book) = crate::model::Book::new(doc) else { return Ok(None) };
    let (original, translation, alignment) = load_entry(client, &book, 0)?;
    Ok(Some((book, original, translation, alignment)))
}

/// Both sides of entry `at`: a side without that chapter is `None`.
/// Both sides of entry `at` (a side without that chapter is `None`), and the
/// chapter's paragraph index checked against both texts.
fn load_entry(client: &Client, book: &crate::model::Book, at: usize) -> Result<(Option<String>, Option<String>, crate::model::Paired), memex_client::ClientError> {
    use crate::model::Paired;
    let (original, translation) = book.keys(at);
    let original = original.map(|k| client.reader_chapter(&book.doc, "original", &k)).transpose()?;
    let translation = translation.map(|k| client.reader_chapter(&book.doc, "translation", &k)).transpose()?;
    let alignment = match (book.paired_key(at), &original, &translation) {
        (Some(key), Some(o), Some(t)) => match client.reader_alignment(&book.doc, &key) {
            Ok(a) => match a.validate(o, Some(t)) {
                Ok(()) => Paired::Index(a),
                Err(e) => Paired::Invalid(e.message()),
            },
            Err(memex_client::ClientError::Unsupported) => Paired::Unsupported,
            Err(e) if e.is_busy() => Paired::Busy,
            Err(e) => Paired::Invalid(e.message()),
        },
        _ => Paired::NotApplicable,
    };
    Ok((original, translation, alignment))
}

fn ai_status_dot(state: &ButtonState, cx: &App) -> AnyElement {
    let color = match state {
        ButtonState::NotGenerated => cx.theme().muted_foreground,
        ButtonState::Done => rgb(0x38a56a).into(),
        ButtonState::Generating => rgb(0xe9b637).into(),
        ButtonState::Failed(_) | ButtonState::Stale(_) => rgb(0xe25b65).into(),
    };
    div().relative().flex_none().size(px(16.)).flex().items_center().justify_center()
        .child(div().size(px(7.)).rounded_full().border_1().border_color(color)
            .when(*state != ButtonState::NotGenerated, |d| d.bg(color)))
        .when(*state == ButtonState::Generating, |d| d.child(
            div().absolute().top_0().left_0().child(gpui_kit::component::spinner::Spinner::new()
                .icon(Icon::default().data(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none"><path d="M12 2a10 10 0 1 1-10 10" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>"#))
                .with_size(px(16.)).color(color))))
        .into_any_element()
}

fn seek_reading_rail(list: &ListState, bounds: Bounds<Pixels>, y: Pixels) {
    let rows = list.item_count().saturating_sub(LEAD);
    if rows == 0 || bounds.size.height <= px(0.) { return; }
    let fraction = ((y - bounds.top()) / bounds.size.height).clamp(0., 1.);
    if fraction >= 1. {
        list.scroll_to_end();
    } else {
        let row = ((fraction * rows as f32).floor() as usize).min(rows - 1);
        list.scroll_to(ListOffset { item_ix: LEAD + row, offset_in_item: px(0.) });
    }
}

/// Estimate scroll progress without treating unmeasured rows as zero-height
/// content. The last row must actually fit in the viewport before showing 100%.
fn list_reading_percent(list: &ListState) -> u8 {
    let n = list.item_count();
    let viewport = list.viewport_bounds();
    if n <= LEAD { return 100; }
    let top = list.logical_scroll_top();
    let fraction = list.bounds_for_item(top.item_ix).filter(|b| b.size.height > px(0.)).map_or(0., |b| (top.offset_in_item / b.size.height).clamp(0., 1.));
    let start = if top.item_ix < LEAD { 0. } else { (top.item_ix - LEAD) as f32 + fraction };
    let mut end = start;
    for ix in top.item_ix..n {
        let Some(b) = list.bounds_for_item(ix) else { break };
        if b.top() >= viewport.bottom() { break; }
        if ix == n - 1 && b.bottom() <= viewport.bottom() + px(0.5) { return 100; }
        if ix >= LEAD && b.size.height > px(0.) {
            end = (ix - LEAD) as f32 + ((viewport.bottom() - b.top()) / b.size.height).clamp(0., 1.);
        }
    }
    reading_percent(start, (n - LEAD) as f32 - (end - start)).min(99)
}

/// Element-id suffix for an AI button, so a state change drops a tooltip that
/// GPUI would otherwise keep showing while the pointer rests on the button.
fn tooltip_key(state: &ButtonState) -> usize {
    match state {
        ButtonState::NotGenerated => 0,
        ButtonState::Generating => 1,
        ButtonState::Done => 2,
        ButtonState::Failed(_) => 3,
        ButtonState::Stale(_) => 4,
    }
}
