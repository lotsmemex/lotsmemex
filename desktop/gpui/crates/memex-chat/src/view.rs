//! Chat views: the transcript (lower half of the center split) and the bottom
//! composer with suggestion chips and the suggestion toggle. `item_chat` runs on
//! the background executor with its own long deadline; the UI never waits on it.

use crate::surface::{self, BUBBLE_RADIUS, FIELD_RADIUS};
use crate::model::{probe, Availability, History, ChatError, ChatModel, Send, Speaker, NO_CHAT_MODEL, NO_CHAT_SUPPORT, SAMPLE_LABEL};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    switch::Switch,
    text::TextView,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::{i18n::{tx_in, Lang}, relabel, tx, txf, Citation, Client, ItemChatRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    CitationClicked(Citation),
    ConversationChanged,
    /// No chat model is configured: open settings.
    OpenSettings,
    /// A question was taken (the shell shows a collapsed conversation again).
    Sent,
    /// The pane header's chevron: fold or unfold the conversation.
    ToggleCollapsed,
}

pub struct Chat {
    client: Client,
    pub model: ChatModel,
    composer: Entity<TextareaState>,
    avatar: std::sync::Arc<Image>,
    scroll: ScrollHandle,
    /// Language the composer placeholder was last set in.
    placeholder_lang: Lang,
    /// 清空 was pressed; the header asks for confirmation.
    confirm_clear: bool,
    _subscriptions: Vec<Subscription>,
}

/// Shown until the capability probe answers (a key).
const CHECKING: &str = "正在检查对话功能…";

/// The sample answer's label in the current language, whichever it was built in.
fn localized_sample(text: &str) -> String {
    for lang in [Lang::Zh, Lang::En] {
        if let Some(rest) = text.strip_prefix(tx_in(lang, SAMPLE_LABEL)) {
            return format!("{}{rest}", tx(SAMPLE_LABEL));
        }
    }
    relabel(text)
}

impl EventEmitter<ChatEvent> for Chat {}

impl Chat {
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Enter sends, Shift+Enter breaks the line.
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 6).submit_on_enter(true).placeholder(tx("就这篇文章提问…")));
        let sub = cx.subscribe_in(&composer, window, |this, state, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                // An Enter that only commits IME composition is not a send.
                let composing = state.update(cx, |s, cx| s.marked_text_range(window, cx)).is_some();
                if !composing {
                    this.submit(window, cx);
                }
            }
        });
        let avatar = std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../assets/memex-avatar.png").to_vec()));
        let mut this = Self {
            client,
            model: ChatModel::new(Availability::Unavailable(CHECKING.into())),
            composer,
            avatar,
            scroll: ScrollHandle::new(),
            placeholder_lang: memex_client::i18n::language(),
            confirm_clear: false,
            _subscriptions: vec![sub],
        };
        this.probe(cx);
        this
    }

    /// Reads the agent's capabilities again (after settings or agent changes).
    pub fn probe(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { (probe(&client), client.capabilities().is_ok_and(|c| c.item_chat_history)) });
        cx.spawn(async move |this, cx| {
            let (availability, history) = task.await;
            this.update(cx, |this, cx| {
                this.model.set_availability(availability);
                this.model.set_history_supported(history);
                this.load_history(cx);
                cx.emit(ChatEvent::ConversationChanged);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn open(&mut self, item_id: &str, revision_id: &str, markdown: &str, sample: bool, cx: &mut Context<Self>) {
        if matches!(self.model.availability(), Availability::Unavailable(r) if r != CHECKING && r != NO_CHAT_SUPPORT && r != NO_CHAT_MODEL) {
            // A transport failure at the last probe: try again with this item.
            self.probe(cx);
        }
        self.confirm_clear = false;
        self.model.open(item_id, revision_id, markdown, sample);
        self.load_history(cx);
        self.scroll_to_latest();
        cx.emit(ChatEvent::ConversationChanged);
        cx.notify();
    }

    /// Reads the open item's stored conversation once (`item_chat_history_v1`),
    /// off the UI thread; an answer for a load that is no longer awaited is dropped.
    fn load_history(&mut self, cx: &mut Context<Self>) {
        if let Some(load) = self.model.history_to_load() {
            self.read_history(load, cx);
        }
    }

    /// 重试 after a failed read of the stored conversation.
    pub fn retry_history(&mut self, cx: &mut Context<Self>) {
        if let Some(load) = self.model.retry_history() {
            self.read_history(load, cx);
            cx.notify();
        }
    }

    fn read_history(&mut self, (item_id, token): (String, u64), cx: &mut Context<Self>) {
        let client = self.client.clone();
        let item = item_id.clone();
        let task = cx.background_executor().spawn(async move { client.conversation_read(&item) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.model.finish_history(&item_id, token, result);
                this.scroll_to_latest();
                cx.emit(ChatEvent::ConversationChanged);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Brings the newest message into view (new messages follow automatically).
    pub fn scroll_to_latest(&self) {
        self.scroll.scroll_to_bottom();
    }

    /// The composer's text state (the platform input handler targets it; tests
    /// and development walks drive IME composition through it).
    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    /// Brings the first message into view.
    pub fn scroll_to_first(&self) {
        self.scroll.set_offset(point(px(0.), px(0.)));
    }

    /// Whether the transcript is scrolled to its last message, once laid out.
    pub fn at_latest(&self) -> bool {
        let max = self.scroll.max_offset().y;
        max <= px(1.) || (-self.scroll.offset().y) >= max - px(1.)
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.model.close();
        cx.emit(ChatEvent::ConversationChanged);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if self.send(&text, cx) {
            self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    /// Sends `text` for the open item; `false` when nothing was taken.
    pub fn send(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let taken = match self.model.send(text) {
            Send::Request(request) => {
                self.dispatch(request, cx);
                true
            }
            Send::Local => true,
            Send::Nothing => false,
        };
        if taken {
            cx.emit(ChatEvent::Sent);
        }
        self.scroll_to_latest();
        cx.emit(ChatEvent::ConversationChanged);
        cx.notify();
        taken
    }

    /// 清空: asks first; only the in-memory history of the open item goes.
    pub fn ask_clear(&mut self, cx: &mut Context<Self>) {
        self.confirm_clear = true;
        cx.notify();
    }

    pub fn cancel_clear(&mut self, cx: &mut Context<Self>) {
        self.confirm_clear = false;
        cx.notify();
    }

    pub fn confirm_clear(&mut self, cx: &mut Context<Self>) {
        self.confirm_clear = false;
        if self.model.clear() {
            cx.emit(ChatEvent::ConversationChanged);
        }
        cx.notify();
    }

    /// The conversation pane's header strip: 「对话 · n」, 清空 (with a
    /// confirmation) and the fold chevron. Rendered by the shell above the
    /// transcript, or alone above the composer when folded.
    pub fn render_header(&mut self, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let count = self.model.question_count();
        let waiting = self.model.is_waiting();
        h_flex()
            .id("conversation-header")
            .flex_none()
            .h(px(32.))
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(Icon::new(IconName::PanelBottom).xsmall().text_color(theme.primary))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme.foreground).child(txf("对话 · {0}", &[&count])))
            .when(waiting && collapsed, |d| d.child(h_flex().gap_1().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(tx("正在思考…"))))
            .child(div().flex_1())
            .map(|d| {
                if self.confirm_clear {
                    d.child(div().text_color(theme.foreground).child(tx("清空这篇的对话记录？（仅本次会话，不可恢复）")))
                        .child(Button::new("clear-cancel").ghost().xsmall().label(tx("取消")).on_click(cx.listener(|this, _, _, cx| this.cancel_clear(cx))))
                        .child(Button::new("clear-confirm").danger().xsmall().label(tx("清空")).on_click(cx.listener(|this, _, _, cx| this.confirm_clear(cx))))
                } else {
                    d.child(
                        Button::new("clear-ask")
                            .ghost()
                            .xsmall()
                            .label(tx("清空"))
                            .disabled(waiting)
                            .tooltip(tx("清空这篇的对话记录"))
                            .on_click(cx.listener(|this, _, _, cx| this.ask_clear(cx))),
                    )
                }
            })
            .child(
                Button::new("conversation-fold")
                    .ghost()
                    .xsmall()
                    .icon(if collapsed { IconName::ChevronUp } else { IconName::ChevronDown })
                    .tooltip(if collapsed { tx("展开对话 ⌘J") } else { tx("收起对话 ⌘J") })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::ToggleCollapsed))),
            )
            .into_any_element()
    }

    /// Retries a send whose outcome is unknown, with the same request id.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = self.model.retry() {
            self.dispatch(request, cx);
            cx.notify();
        }
    }

    fn dispatch(&mut self, request: ItemChatRequest, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let req = request.clone();
        let task = cx.background_executor().spawn(async move { client.item_chat(&req) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.model.finish(&request, result);
                this.scroll_to_latest();
                cx.emit(ChatEvent::ConversationChanged);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Composer row; rendered by the shell at the bottom of the center area.
    pub fn render_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lang = memex_client::i18n::language();
        if self.placeholder_lang != lang {
            self.placeholder_lang = lang;
            self.composer.update(cx, |s, cx| s.set_placeholder(tx("就这篇文章提问…"), window, cx));
        }
        let theme = cx.theme().clone();
        let can_send = self.model.can_send();
        let availability = self.model.availability().clone();
        let fixture = self.client.mode().is_fixture();
        let reason = match &availability {
            Availability::Unavailable(r) if r == NO_CHAT_SUPPORT || r == NO_CHAT_MODEL || r == CHECKING => Some(relabel(r)),
            Availability::Unavailable(r) => Some(txf("AI 对话暂不可用：{0}", &[&relabel(r)])),
            _ => None,
        };
        let needs_model = availability == Availability::Unavailable(NO_CHAT_MODEL.into());
        // The stored conversation's load, said plainly (it gates sending while loading).
        let history = self.model.history();
        let history_line = match &history {
            History::Loading(_) => Some(h_flex().gap_1().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(tx("正在载入对话记录…")).into_any_element()),
            History::Failed(reason) => Some(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                    .child(txf("对话记录载入失败：{0}", &[reason]))
                    .child(Button::new("history-retry").ghost().xsmall().icon(IconName::RefreshCw).label(tx("重试")).on_click(cx.listener(|this, _, _, cx| this.retry_history(cx))))
                    .into_any_element(),
            ),
            History::Loaded { empty: true } if !self.model.has_conversation() => Some(div().child(tx("这篇还没有对话记录")).into_any_element()),
            _ => None,
        };
        let mut chips = h_flex().gap_2().flex_wrap();
        for (i, s) in self.model.suggestions().iter().enumerate() {
            let text = tx(s).to_string();
            chips = chips.child(
                div()
                    .id(("suggest", i))
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .hover(|d| d.bg(theme.secondary))
                    .child(text.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(&text, cx);
                    })),
            );
        }
        let sample = availability == Availability::Sample;
        let tone = surface::for_theme(theme.background);
        // The composer zone is part of the chat surface: its tone, a rose rule on top.
        v_flex()
            .gap_2()
            .px_4()
            .pb_3()
            .pt_3()
            .bg(tone.surface)
            .border_t_1()
            .border_color(tone.edge)
            .when(sample && can_send, |d| {
                d.child(
                    // The toggle sits beside the chips when there is room and wraps to
                    // its own right-aligned line when there is not (longer English chips).
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .child(chips)
                        .child(
                            div().flex_none().ml_auto().child(
                                Switch::new("suggestions")
                                    .label(tx("建议提问"))
                                    .checked(self.model.suggestions_enabled)
                                    .small()
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.model.suggestions_enabled = *checked;
                                        cx.notify();
                                    })),
                            ),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .p_1()
                    .rounded(px(FIELD_RADIUS))
                    .border_1()
                    .border_color(if can_send { theme.primary.opacity(0.45) } else { tone.field_border })
                    .bg(tone.field)
                    .child(div().flex_1().min_w_0().child(Textarea::new(&self.composer).appearance(false).disabled(!can_send)))
                    .child(
                        Button::new("send")
                            .icon(IconName::ArrowUp)
                            .primary()
                            .disabled(!can_send)
                            .tooltip(tx("发送（Enter）；换行用 Shift+Enter"))
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .when_some(history_line, |d, line| d.child(h_flex().text_xs().text_color(theme.muted_foreground).child(line)))
            .when(reason.is_some() || needs_model, |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .children(reason)
                        .when(needs_model, |d| {
                            d.child(
                                Button::new("chat-settings")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Settings)
                                    .label(tx("去设置对话模型"))
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::OpenSettings))),
                            )
                        }),
                )
            })
            .when(sample, |d| d.child(div().text_xs().text_color(theme.muted_foreground).child(tx("开发模式：合成数据与示例对话，未连接任何模型"))))
            .when(fixture && availability == Availability::Ready, |d| {
                d.child(div().text_xs().text_color(theme.muted_foreground).child(tx("开发模式：合成数据与合成回复，未连接任何模型")))
            })
            .into_any_element()
    }

    fn avatar(&self, cx: &App) -> Img {
        img(self.avatar.clone()).flex_none().size(px(32.)).rounded_full().border_1().border_color(cx.theme().primary.opacity(0.5))
    }
}

impl Render for Chat {
    /// Transcript only; empty when there is no conversation.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tone = surface::for_theme(theme.background);
        // 「从截图恢复」 on restored turns; a note on a turn the agent cut to fit a page.
        let provenance = |m: &crate::model::Message| -> Option<AnyElement> {
            (m.restored || m.cut).then(|| {
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when(m.restored, |d| {
                        d.child(
                            h_flex()
                                .gap_1()
                                .px_1p5()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(tone.reply_border)
                                .child(Icon::new(IconName::Inbox).xsmall())
                                .child(tx("从截图恢复")),
                        )
                    })
                    .when(m.cut, |d| d.child(tx("这条记录过长，只显示了前一部分")))
                    .into_any_element()
            })
        };
        let mut col = v_flex().id("transcript").size_full().overflow_y_scroll().track_scroll(&self.scroll).gap_3().p_4();
        let memex_row = |avatar: Img, body: AnyElement| {
            h_flex()
                .items_start()
                .gap_2()
                .child(avatar)
                .child(v_flex().flex_1().min_w_0().max_w(px(640.)).gap_1().child(div().text_xs().text_color(theme.muted_foreground).child(memex_client::ASSISTANT_NAME)).child(body))
        };
        for (i, m) in self.model.transcript().iter().enumerate() {
            let bubble = match m.speaker {
                Speaker::User => h_flex().justify_end().child(
                    v_flex()
                        .items_end()
                        .gap_1()
                        .min_w_0()
                        .max_w(px(520.))
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(px(BUBBLE_RADIUS))
                                .bg(tone.question)
                                .text_sm()
                                .when(m.unanswered, |d| d.text_color(theme.muted_foreground))
                                .child(m.text.clone()),
                        )
                        .when(m.unanswered, |d| d.child(div().text_xs().text_color(theme.muted_foreground).child(tx("未获回复"))))
                        .children(provenance(m)),
                ),
                Speaker::System => h_flex().justify_center().child(div().text_xs().text_color(theme.muted_foreground).child(relabel(&m.text))),
                Speaker::Assistant => {
                    let mut cites = h_flex().gap_2().pt_1();
                    for (j, c) in m.citations.iter().enumerate() {
                        let cite = c.clone();
                        cites = cites.child(
                            div()
                                .id(("cite", i * 100 + j))
                                .px_2()
                                .rounded(px(6.))
                                .bg(theme.primary.opacity(0.12))
                                .text_color(theme.primary)
                                .text_xs()
                                .child(format!("{} ›", c.label))
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::CitationClicked(cite.clone())))),
                        );
                    }
                    let text: AnyElement = if m.sample {
                        div().child(localized_sample(&m.text)).into_any_element()
                    } else {
                        TextView::markdown(("reply", i), memex_client::markdown::safe_markdown(&m.text)).selectable(true).into_any_element()
                    };
                    let body = div()
                        .px_3()
                        .py_2()
                        .rounded(px(BUBBLE_RADIUS))
                        .border_1()
                        .border_color(tone.reply_border)
                        .bg(tone.reply)
                        .text_sm()
                        .line_height(relative(1.7))
                        .child(text)
                        .when(!m.citations.is_empty(), |d| d.child(cites))
                        .when(m.truncated, |d| d.child(div().pt_1().text_xs().text_color(theme.muted_foreground).child(tx("回复达到长度上限，已截断"))))
                        .into_any_element();
                    let body = match provenance(m) {
                        Some(label) => v_flex().gap_1().child(body).child(label).into_any_element(),
                        None => body,
                    };
                    memex_row(self.avatar(cx), body)
                }
            };
            col = col.child(bubble);
        }
        if self.model.is_waiting() {
            let body = h_flex()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(px(BUBBLE_RADIUS))
                .border_1()
                .border_color(tone.reply_border)
                .bg(tone.reply.opacity(0.7))
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Loader).small().text_color(theme.primary))
                .child(tx("正在思考…"))
                .into_any_element();
            col = col.child(memex_row(self.avatar(cx), body));
        }
        if let Some(error) = self.model.error().cloned() {
            let (text, action): (String, Option<AnyElement>) = match error {
                ChatError::NoProvider => (
                    tx("还没有配置对话模型").into(),
                    Some(Button::new("chat-error-settings").small().outline().icon(IconName::Settings).label(tx("去设置对话模型")).on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::OpenSettings))).into_any_element()),
                ),
                ChatError::Unknown => (
                    tx("回复状态未知，可重试").into(),
                    Some(Button::new("chat-retry").small().primary().icon(IconName::RefreshCw).label(tx("重试")).on_click(cx.listener(|this, _, _, cx| this.retry(cx))).into_any_element()),
                ),
                ChatError::Failed(reason) => (txf("发送失败：{0}", &[&relabel(&reason)]), None),
            };
            col = col.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .ml(px(40.))
                    .max_w(px(640.))
                    .px_3()
                    .py_2()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme.warning.opacity(0.35))
                    .bg(theme.warning.opacity(0.08))
                    .text_sm()
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                    .child(div().flex_1().min_w_0().child(text))
                    .children(action),
            );
        }
        col
    }
}
