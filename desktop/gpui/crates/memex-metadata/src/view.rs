//! Metadata card: editable title and manual tags (unchanged writer), the
//! bibliographic fields (`biblio_view`, editable only when the agent advertises
//! `bibliographic_metadata_v2`), the two dates (发布日期 in the bibliographic
//! block, 采集日期 = `added_at` only), analysis state and summary, Save/Cancel,
//! conflict lock and error messages.

use crate::biblio_view::{fetch, BiblioEditor, BiblioEvent, Loaded};
use crate::session::{EditSession, EditStatus};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::biblio::added_at_local;
use memex_client::{relabel, source_display, tx, txf, Client, ItemDetail, Lang};
use std::{cell::Cell, rc::Rc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataEvent {
    /// A new revision was saved; other modules should reload this item.
    Saved { item_id: String },
    /// Read this item's AI 纪要 in the reader's full reading area.
    ReadMinutes,
    /// 生成: ask for this item's AI 纪要 (`minutes_request`, through the reader).
    GenerateMinutes,
}

/// Display facts that come from the list row, not the editable record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemFacts {
    pub item_type: String,
    pub source: String,
    /// 采集日期 as listed (`added_at`, RFC3339 UTC); `None` is "not recorded".
    pub added_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Lower half of the right panel in reading view.
    Compact,
    /// Classic inspector with title header and summary.
    Inspector,
}

/// 采集日期 in local time, or 未记录. Only `added_at`: the capture time is
/// assigned before commit and is not evidence of entering the library.
pub fn captured_label(added_at: Option<&str>) -> String {
    added_at_local(added_at).unwrap_or_else(|| tx("未记录").into())
}

pub struct Metadata {
    client: Client,
    pub session: EditSession,
    detail: Option<ItemDetail>,
    pub biblio: Entity<BiblioEditor>,
    facts: ItemFacts,
    loading: bool,
    load_error: Option<String>,
    /// Outcome of the last save, kept across the reload it triggers.
    saved_note: Option<(String, String)>,
    ticket: u64,
    title: Entity<InputState>,
    tag: Entity<InputState>,
    sync_title: bool,
    /// Width of the title input, from the last layout (for the tooltip).
    title_width: Rc<Cell<Pixels>>,
    /// Language the input placeholders were last set in.
    placeholder_lang: Lang,
    pub layout: Layout,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MetadataEvent> for Metadata {}

impl Metadata {
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder(tx("标题")));
        let tag = cx.new(|cx| InputState::new(window, cx).placeholder(tx("添加标签")));
        let biblio = cx.new(|_| BiblioEditor::new(client.clone()));
        let subscriptions = vec![
            cx.subscribe_in(&title, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) && !this.sync_title {
                    let value = state.read(cx).value().to_string();
                    if value != this.session.title {
                        this.session.set_title(&value);
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(&tag, window, |this, state, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let value = state.read(cx).value().to_string();
                    this.session.add_tag(&value);
                    state.update(cx, |s, cx| s.set_value("", window, cx));
                    cx.notify();
                }
            }),
            cx.observe(&biblio, |_, _, cx| cx.notify()),
            cx.subscribe(&biblio, |this, _, event: &BiblioEvent, cx| this.biblio_event(event.clone(), cx)),
        ];
        Self {
            client,
            session: EditSession::default(),
            detail: None,
            biblio,
            facts: ItemFacts::default(),
            loading: false,
            load_error: None,
            saved_note: None,
            ticket: 0,
            title,
            tag,
            sync_title: false,
            title_width: Rc::new(Cell::new(px(0.))),
            placeholder_lang: memex_client::i18n::language(),
            layout: Layout::Compact,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Scrolls the panel to its action row (save/cancel).
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn blocks_navigation(&self, cx: &App) -> bool {
        self.session.blocks_navigation() || self.biblio.read(cx).blocks_navigation()
    }

    pub fn item_id(&self) -> Option<&str> {
        self.session.item_id()
    }

    /// Loads `item_id`; responses for an earlier request are dropped.
    pub fn load(&mut self, item_id: Option<String>, facts: ItemFacts, cx: &mut Context<Self>) {
        self.ticket += 1;
        let ticket = self.ticket;
        if self.saved_note.as_ref().map(|(id, _)| Some(id)) != Some(item_id.as_ref()) {
            self.saved_note = None;
        }
        if self.session.item_id() != item_id.as_deref() {
            // Another item starts at the top of the panel.
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.facts = facts;
        self.detail = None;
        self.load_error = None;
        self.session.clear();
        self.biblio.update(cx, |b, cx| b.clear(cx));
        self.sync_title = true;
        let Some(item_id) = item_id else {
            self.loading = false;
            cx.notify();
            return;
        };
        self.loading = true;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let detail = client.item_detail(&item_id);
            // Bound to the same revision as the editable record.
            let biblio: Option<Loaded> = detail.as_ref().ok().map(|d| fetch(&client, &item_id, &d.canonical.revision_id));
            (detail, biblio)
        });
        cx.spawn(async move |this, cx| {
            let (result, loaded) = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(detail) => {
                        this.session.begin(&detail);
                        if let Some(loaded) = loaded {
                            let (id, rev) = (detail.canonical.item_id.clone(), detail.canonical.revision_id.clone());
                            this.biblio.update(cx, |b, cx| b.set_loaded(&id, &rev, loaded, cx));
                        }
                        let minutes = detail.analysis.ai_minutes.clone();
                        this.biblio.update(cx, |b, _| b.minutes = minutes);
                        this.detail = Some(detail);
                    }
                    Err(e) => this.load_error = Some(e.message()),
                }
                this.sync_title = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let id = self.session.item_id().map(str::to_string).or_else(|| self.detail.as_ref().map(|d| d.canonical.item_id.clone()));
        let facts = self.facts.clone();
        self.load(id, facts, cx);
    }

    /// Replaces the title draft, as typing into the title field does.
    pub fn edit_title(&mut self, value: &str, cx: &mut Context<Self>) {
        self.session.set_title(value);
        self.sync_title = true;
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.session.status() == EditStatus::Conflict {
            self.reload(cx);
            return;
        }
        self.session.cancel();
        self.biblio.update(cx, |b, cx| b.reset(cx));
        self.sync_title = true;
        cx.notify();
    }

    fn can_save(&self, cx: &App) -> bool {
        let b = self.biblio.read(cx);
        let biblio = b.is_dirty() && !b.is_saving() && self.session.can_edit();
        (self.session.can_save() || biblio) && (!self.session.is_dirty() || self.session.can_save())
    }

    /// Title/tags through the existing writer first, then the bibliographic
    /// edit against the revision that save produced. Each part reports its own
    /// outcome; nothing is reported as saved that the agent did not confirm.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        let item_id = self.session.item_id().unwrap_or_default().to_string();
        if !self.session.is_dirty() {
            let rev = self.session.revision_id().unwrap_or_default().to_string();
            self.session.set_message(None);
            self.biblio.update(cx, |b, cx| b.save(rev, true, cx));
            cx.notify();
            return;
        }
        let Some(request) = self.session.start_save() else { return };
        cx.notify();
        let client = self.client.clone();
        let req = request.clone();
        let task = cx.background_executor().spawn(async move {
            client.update_metadata(&req.item_id, &req.expected_revision_id, &req.title, &req.tags).map(|u| u.revision_id)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let saved = result.as_ref().ok().cloned();
                this.session.finish_save(&request, result);
                this.sync_title = true;
                if let Some(rev) = saved {
                    // The bibliographic part goes against the revision just written.
                    let started = this.biblio.update(cx, |b, cx| b.is_dirty() && b.save(rev, false, cx));
                    if !started {
                        cx.emit(MetadataEvent::Saved { item_id: item_id.clone() });
                        this.reload(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn biblio_event(&mut self, event: BiblioEvent, cx: &mut Context<Self>) {
        let item_id = self.session.item_id().unwrap_or_default().to_string();
        match event {
            BiblioEvent::Saved { revision_id, note } => {
                self.session.advance_revision(revision_id);
                self.saved_note = Some((item_id.clone(), note.unwrap_or_else(|| tx("已保存").into())));
                cx.emit(MetadataEvent::Saved { item_id });
                self.reload(cx);
            }
            BiblioEvent::ReadMinutes => cx.emit(MetadataEvent::ReadMinutes),
            BiblioEvent::GenerateMinutes => cx.emit(MetadataEvent::GenerateMinutes),
            BiblioEvent::Conflict(message) => self.session.mark_conflict(message),
            BiblioEvent::Unchanged => {
                self.saved_note = Some((item_id, tx("没有需要保存的修改").into()));
                self.reload(cx);
            }
            BiblioEvent::Rejected(message) | BiblioEvent::Failed(message) => self.session.set_message(Some(message)),
        }
        cx.notify();
    }
}

pub(crate) fn value_box(value: Option<String>, cx: &App) -> Div {
    let theme = cx.theme();
    let empty = value.is_none();
    div()
        .px_2()
        .py(px(3.))
        .min_h(px(26.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(0.45))
        .when(empty, |d| d.text_color(theme.muted_foreground))
        .child(value.unwrap_or_else(|| "—".into()))
}

/// AI 纪要 in the side panel: the text when succeeded, else one state line.
/// `None` when the agent does not report minutes. `generate` (manual minutes)
/// sits beside 未生成.
pub(crate) fn minutes_field(minutes: Option<&memex_client::AiMinutes>, generate: Option<AnyElement>, cx: &App) -> Option<AnyElement> {
    use memex_client::MinutesState;
    let m = minutes?;
    let theme = cx.theme();
    let value = match (m.text(), m.state_line()) {
        (Some(text), _) => div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.primary.opacity(0.18))
            .bg(theme.primary.opacity(0.05))
            .text_color(theme.foreground)
            .line_height(relative(1.6))
            .child(gpui_kit::component::text::TextView::markdown("minutes-panel", memex_client::markdown::safe_markdown(&memex_client::minutes_markdown(text))).selectable(true)),
        (None, Some(line)) => div().pt(px(2.)).text_color(if m.state == MinutesState::Failed { theme.warning } else { theme.muted_foreground }).child(line),
        (None, None) => div().child(h_flex().gap_2().items_center().child(div().text_color(theme.muted_foreground).child(tx("未生成"))).children(generate)),
    };
    Some(field(tx("AI 纪要"), value, cx).into_any_element())
}

pub(crate) fn field(label: &str, value: impl IntoElement, cx: &App) -> Div {
    // English labels ("Translation", "Published") need a wider column.
    let width = if memex_client::i18n::language() == Lang::En { 84. } else { 64. };
    h_flex()
        .gap_3()
        .items_start()
        .child(div().w(px(width)).flex_shrink_0().pt(px(2.)).text_color(cx.theme().muted_foreground).child(label.to_string()))
        .child(div().flex_1().min_w_0().child(value))
}

/// Whether `text` overflows an input of `width` (inner padding included).
fn overflows(text: &str, width: Pixels, window: &Window) -> bool {
    if width <= px(0.) || text.is_empty() {
        return false;
    }
    let style = window.text_style();
    let size = px(13.);
    let line = text.replace('\n', " ");
    let run = TextRun { len: line.len(), font: style.font(), color: style.color, background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(line.into(), size, &[run], None).width > width - px(18.)
}

impl Render for Metadata {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sync_title {
            self.sync_title = false;
            let title = self.session.title.clone();
            self.title.update(cx, |s, cx| s.set_value(title, window, cx));
        }
        let lang = memex_client::i18n::language();
        if self.placeholder_lang != lang {
            self.placeholder_lang = lang;
            self.title.update(cx, |s, cx| s.set_placeholder(tx("标题"), window, cx));
            self.tag.update(cx, |s, cx| s.set_placeholder(tx("添加标签"), window, cx));
        }
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let Some(detail) = self.detail.clone() else {
            let message = if self.loading { tx("正在载入…").to_string() } else { self.load_error.as_deref().map(relabel).unwrap_or_else(|| tx("选择一个条目查看信息").into()) };
            return v_flex().size_full().items_center().justify_center().text_sm().text_color(muted).child(message).into_any_element();
        };
        let status = self.session.status();
        let locked = !self.session.can_edit();
        self.biblio.update(cx, |b, _| b.locked = locked);
        let tags = self.session.tags.clone();
        let analysis = detail.analysis.clone();
        // 中译标题: read-only, as soon as the title step succeeded (even if the
        // document did not), when it differs from the title.
        let title_zh = analysis.chinese_title(&detail.canonical.title);
        let inspector = self.layout == Layout::Inspector;
        let serif = SharedString::from(memex_client::reading_family());

        let mut tag_row = h_flex().flex_wrap().gap_1();
        for (i, tag) in tags.iter().enumerate() {
            let t = tag.clone();
            tag_row = tag_row.child(
                h_flex()
                    .id(("tag", i))
                    .gap_1()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(6.))
                    .bg(theme.list_active)
                    .text_color(if locked { muted } else { theme.primary })
                    .text_xs()
                    .child(tag.clone())
                    .when(!locked, |d| {
                        d.child(
                            div()
                                .id(("tag-remove", i))
                                .child(Icon::new(IconName::Close).xsmall())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.session.remove_tag(&t);
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        if !locked {
            tag_row = tag_row.child(div().w(px(120.)).child(Input::new(&self.tag).xsmall()));
        }

        let biblio = self.biblio.read(cx);
        let biblio_absent = biblio.original().is_none();
        let biblio_dirty = biblio.is_dirty();
        let biblio_saving = biblio.is_saving();
        let biblio_note = biblio.note().map(relabel);
        // 采集日期: `added_at` only, from the record when read, else from the list row.
        let added_at = biblio.original().and_then(|b| b.added_at.clone()).or_else(|| self.facts.added_at.clone());
        let message = self
            .session
            .message()
            .map(relabel)
            .or_else(|| self.saved_note.as_ref().filter(|(id, _)| Some(id.as_str()) == self.session.item_id()).map(|(_, n)| relabel(n)));
        let conflict = status == EditStatus::Conflict;
        let saving = status == EditStatus::Saving || biblio_saving;

        // Full title on hover, only when the field cuts it off.
        let title_text = self.session.title.clone();
        let truncated = overflows(&title_text, self.title_width.get(), window);
        let width_cell = self.title_width.clone();
        let title_field = div()
            .id("title-field")
            .relative()
            .child(Input::new(&self.title).disabled(locked))
            .child(canvas(move |bounds, _, _| width_cell.set(bounds.size.width), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .when(truncated, |d| d.tooltip(move |window, cx| Tooltip::new(title_text.clone()).build(window, cx)));

        v_flex()
            .id("metadata")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_3()
            .gap_3()
            .text_sm()
            .when(inspector, |d| {
                d.child(div().font_family(serif.clone()).text_lg().font_weight(FontWeight::SEMIBOLD).child(detail.canonical.title.clone()))
            })
            .when(!inspector, |d| d.child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(muted).child(tx("条目信息"))))
            .child(field(tx("类型"), div().child(memex_client::item_type_label(&self.facts.item_type)), cx))
            .child(field(tx("标题"), title_field, cx))
            .when_some(title_zh, |d, t| d.child(field(tx("中译标题"), value_box(Some(t), cx), cx)))
            .when_some(biblio_note, |d, note| {
                d.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .px_2()
                        .py_1()
                        .rounded(px(6.))
                        .bg(theme.accent.opacity(0.6))
                        .text_xs()
                        .text_color(muted)
                        .child(Icon::new(IconName::Info).xsmall().text_color(theme.primary))
                        .child(div().flex_1().min_w_0().child(note)),
                )
            })
            .child(self.biblio.clone())
            // Without bibliographic fields the minutes still show, below the title.
            .when(biblio_absent, |d| d.children(minutes_field(analysis.ai_minutes.as_ref(), None, cx)))
            .child(field(tx("采集日期"), value_box(Some(captured_label(added_at.as_deref())), cx).when(added_at.is_none(), |d| d.text_color(muted)), cx))
            .child(field(tx("来源"), div().child(source_display(&self.facts.source).to_string()), cx))
            .child(field(tx("翻译状态"), div().px_2().rounded(px(6.)).bg(theme.secondary).text_xs().child(translation_state_text(&analysis)), cx))
            .when(inspector, |d| {
                d.child(field(
                    tx("摘要"),
                    div().text_color(theme.foreground).child(analysis.summary.clone().unwrap_or_else(|| txf("（{0}）", &[&analysis.summary_state.label()]))),
                    cx,
                ))
            })
            .child(field(tx("标签"), tag_row, cx))
            .when_some(message, |d, m| {
                d.child(
                    h_flex()
                        .gap_2()
                        .p_2()
                        .rounded(px(6.))
                        .when(conflict, |d| d.items_start().bg(theme.warning.opacity(0.12)).text_color(theme.foreground))
                        .when(!conflict, |d| d.text_color(muted))
                        .when(conflict, |d| d.child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning)))
                        .child(div().flex_1().min_w_0().child(m)),
                )
            })
            .when((self.session.is_dirty() || biblio_dirty) && !conflict, |d| d.child(div().text_xs().text_color(theme.primary).child(tx("有未保存修改"))))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel")
                            .label(if conflict { tx("取消并重新载入") } else { tx("取消") })
                            .when(conflict, |b| b.primary())
                            .disabled(!(self.session.can_cancel() || (biblio_dirty && !saving)))
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .child(
                        Button::new("save")
                            .label(if saving { tx("保存中…") } else { tx("保存") })
                            .when(!conflict, |b| b.primary())
                            .disabled(saving || !self.can_save(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::captured_label;
    use memex_client::tx;

    #[test]
    fn captured_date_is_added_at_only() {
        assert_eq!(captured_label(None), tx("未记录"));
        assert_eq!(captured_label(Some("not a time")), tx("未记录"));
        let shown = captured_label(Some("2026-10-28T06:02:00Z"));
        assert!(shown.starts_with("2026-10-2") && shown.len() == 16, "{shown}");
    }
}

/// The translation state with its reason in plain words (from the agent, when it
/// gives one).
fn translation_state_text(analysis: &memex_client::TranslationItem) -> String {
    use memex_client::{translation_reason_label, AnalysisState};
    use memex_client::{translating_label, unfinished_label};
    let progress = analysis.translation_progress.as_ref();
    match (analysis.translation_state, analysis.translation_reason.as_deref()) {
        (_, Some("source_is_chinese")) => translation_reason_label("source_is_chinese"),
        (AnalysisState::Running, _) => translating_label(progress),
        (AnalysisState::Failed, reason) if unfinished_label(progress).is_some() => {
            let label = unfinished_label(progress).unwrap_or_default();
            match reason {
                Some(r) => format!("{label} · {}", translation_reason_label(r)),
                None => label,
            }
        }
        (AnalysisState::Failed, None) => txf("{0}（代理未说明原因）", &[&analysis.translation_state.label()]),
        (AnalysisState::Failed | AnalysisState::Unavailable, Some(reason)) => txf("{0}：{1}", &[&analysis.translation_state.label(), &translation_reason_label(reason)]),
        (state, _) => state.label().to_string(),
    }
}
