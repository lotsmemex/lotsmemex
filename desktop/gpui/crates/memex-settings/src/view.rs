//! Settings window (approved s2): purpose rows in three groups, provider cards with
//! masked credential state, explicit discovery and manual models. All requests go
//! through `memex-client`; the entered key lives only in the input until sent.

use crate::model::{Group, ModelSource, SettingsModel, Status, ROWS};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::reading_stats::{count_text, label as stats_label, CountMethod, StatsPrefs, EN_SPEEDS, sample, ZH_SPEEDS};
use memex_client::{relabel, tx, txf, Client, CredentialState, DiscoverAuthorization, Lang, LanguagePref, Purpose, Secret, REASONING_EFFORTS};

fn effort_label(value: &str) -> String {
    match value {
        "none" => tx("不推理").into(),
        "minimal" => tx("极低").into(),
        "low" => tx("低").into(),
        "medium" => tx("中").into(),
        "high" => tx("高").into(),
        "xhigh" => tx("极高").into(),
        other => txf("未知（{0}）", &[&other]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Appearance,
    Models,
    /// 「版本」: app version and build, the connected agent.
    Version,
    /// 「更新日志」: the portrait over every release, newest first.
    Changelog,
}

/// Raised when the user picks an interface language; the shell applies and saves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    LanguageChanged(LanguagePref),
    ReadingStatsChanged(StatsPrefs),
    NoteFontChanged(memex_client::NoteFont),
    SpeakerNamesChanged(memex_client::SpeakerNames),
}

pub struct Settings {
    client: Client,
    /// The shell's stored interface-language preference.
    pub language: LanguagePref,
    pub reading_stats: StatsPrefs,
    pub note_font: memex_client::NoteFont,
    pub speaker_names: memex_client::SpeakerNames,
    /// Language the input placeholders were last set in.
    placeholder_lang: Lang,
    pub model: SettingsModel,
    pub page: Page,
    ticket: u64,
    key_for: Option<String>,
    key: Entity<InputState>,
    model_for: Option<String>,
    model_label: Entity<InputState>,
    model_id: Entity<InputState>,
    adding_provider: bool,
    provider_label: Entity<InputState>,
    provider_base: Entity<InputState>,
    scroll: ScrollHandle,
    /// The 「版本」 page's facts, read off the UI thread.
    version: Option<crate::version::VersionInfo>,
    /// Re-reads while the agent is still hashing its binary (bounded).
    version_polls: u32,
    avatar: std::sync::Arc<Image>,
    portrait: std::sync::Arc<Image>,
}

impl EventEmitter<SettingsEvent> for Settings {}

const KEY_HINT: &str = "粘贴密钥，仅发送给本机代理保存";
const MODEL_LABEL_HINT: &str = "显示名（可选）";
const MODEL_ID_HINT: &str = "模型 ID，例如 gpt-6-astra";
const PROVIDER_LABEL_HINT: &str = "名称";
const PROVIDER_BASE_HINT: &str = "地址，例如 http://127.0.0.1:8317/v1/";

impl Settings {
    pub fn new(client: Client, language: LanguagePref, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |p: &'static str, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).placeholder(tx(p)));
        let key = cx.new(|cx| InputState::new(window, cx).placeholder(tx(KEY_HINT)).masked(true));
        let mut this = Self {
            client,
            language,
            reading_stats: StatsPrefs::default(),
            note_font: Default::default(),
            speaker_names: Default::default(),
            placeholder_lang: memex_client::i18n::language(),
            model: SettingsModel::default(),
            page: Page::Models,
            ticket: 0,
            key_for: None,
            key,
            model_for: None,
            model_label: input(MODEL_LABEL_HINT, window, cx),
            model_id: input(MODEL_ID_HINT, window, cx),
            adding_provider: false,
            provider_label: input(PROVIDER_LABEL_HINT, window, cx),
            provider_base: input(PROVIDER_BASE_HINT, window, cx),
            scroll: ScrollHandle::new(),
            version: None,
            version_polls: 0,
            // The approved black-rose portrait (the chat avatar).
            avatar: std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../../memex-chat/assets/memex-avatar.png").to_vec())),
            portrait: std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, crate::changelog::PORTRAIT_PNG.to_vec())),
        };
        this.reload(cx);
        this.load_version(cx);
        this
    }

    /// Reads the version facts (hashes the binary) off the UI thread. While the
    /// agent reports its hash as still being computed, reads again a few times.
    pub fn load_version(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { crate::version::read(&client) });
        cx.spawn(async move |this, cx| {
            let info = task.await;
            let hashing = matches!(&info.agent_version, crate::version::AgentVersionInfo::Reported(v) if v.binary_sha256.is_none());
            let again = this
                .update(cx, |this, cx| {
                    this.version = Some(info);
                    cx.notify();
                    this.version_polls += 1;
                    hashing && this.version_polls < 10
                })
                .unwrap_or(false);
            if again {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                this.update(cx, |this, cx| this.load_version(cx)).ok();
            }
        })
        .detach();
    }

    /// Forgets the facts and reads them again (the development walk).
    pub fn reload_version(&mut self, cx: &mut Context<Self>) {
        self.version = None;
        self.version_polls = 0;
        self.load_version(cx);
    }

    /// Whether the 「版本」 facts have been read.
    pub fn version_loaded(&self) -> bool {
        self.version.is_some()
    }

    /// 「更新日志」: the large portrait with the current version, then a timeline of
    /// releases (same entries as the phone, from `mobile/web/changelog.mjs`).
    fn render_changelog(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let all = crate::changelog::releases();
        let cream = hsla(30. / 360., 0.4, 0.96, 1.);
        // The whole 2:3 portrait (never cropped), with the title beside it.
        let portrait = div()
            .relative()
            .flex_none()
            .w(px(300.))
            .h(px(450.))
            .rounded(px(16.))
            .overflow_hidden()
            .border_1()
            .border_color(theme.primary.opacity(0.45))
            .child(img(self.portrait.clone()).absolute().top_0().left_0().w_full().h_full().object_fit(ObjectFit::Cover))
            .child(
                div().absolute().left_0().bottom_0().w_full().h(px(140.)).bg(linear_gradient(
                    180.,
                    linear_color_stop(hsla(345. / 360., 0.3, 0.06, 0.), 0.),
                    linear_color_stop(hsla(345. / 360., 0.3, 0.06, 0.7), 1.),
                )),
            )
            .child(div().absolute().left_0().bottom(px(14.)).w_full().flex().justify_center().text_color(cream).text_2xl().font_weight(FontWeight::SEMIBOLD).font_family("Songti SC").child(memex_client::BRAND_NAME));
        let mut latest = v_flex().w_full().min_w_0().gap_1();
        for t in &all[0].items {
            latest = latest.child(h_flex().gap_2().items_start().text_sm().child(div().text_color(theme.primary).child("✦")).child(div().flex_1().min_w_0().child(t.clone())));
        }
        let hero = h_flex()
            .gap_6()
            .items_end()
            .p_4()
            .rounded(px(20.))
            .border_1()
            .border_color(theme.primary.opacity(0.25))
            .bg(theme.primary.opacity(0.05))
            .child(portrait)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .pb_2()
                    .child(div().text_3xl().font_weight(FontWeight::SEMIBOLD).font_family("Songti SC").text_color(theme.primary).child(memex_client::BRAND_NAME))
                    .child(div().text_sm().text_color(muted).font_family("Songti SC").child(tx("为你珍藏每一篇值得读的文字")))
                    .child(
                        h_flex()
                            .mt_2()
                            .gap_2()
                            .items_center()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("v{}", all[0].version)))
                            .child(div().text_xs().child(txf("共 {0} 次更新", &[&all.len()]))),
                    )
                    .child(div().mt_3().text_xs().text_color(muted).child(format!("{} · {}", all[0].date, crate::changelog::platforms_label(&all[0]))))
                    .child(latest),
            );
        let mut list = v_flex().gap_4().pl_4().ml_1().border_l_2().border_color(theme.primary.opacity(0.3));
        for (i, r) in all.iter().enumerate() {
            let mut items = v_flex().gap_1().pl_1();
            for t in &r.items {
                items = items.child(h_flex().gap_2().items_start().text_sm().child(div().text_color(theme.primary).child("•")).child(div().flex_1().min_w_0().child(t.clone())));
            }
            list = list.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).font_family("Songti SC").text_color(theme.primary).child(format!("v{}", r.version)))
                            .when(i == 0, |d| d.child(div().px_2().rounded_full().bg(theme.primary).text_color(theme.primary_foreground).text_xs().child(tx("当前版本"))))
                            .child(div().text_xs().text_color(muted).child(format!("{} · {}", r.date, crate::changelog::platforms_label(r)))),
                    )
                    .child(items),
            );
        }
        v_flex().gap_5().max_w(px(640.)).child(hero).child(list)
    }

    /// 「版本」: the portrait, the app's version and build, the connected agent.
    fn render_version(&self, cx: &mut Context<Self>) -> Div {
        use crate::version::{AgentInfo, APP_VERSION};
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let unknown = || tx("未知").to_string();
        let row = |label: &'static str, value: String, mono: bool| {
            h_flex()
                .px_4()
                .py_2()
                .gap_3()
                .items_start()
                .border_t_1()
                .border_color(theme.border)
                .child(div().w(px(120.)).flex_none().text_color(muted).child(tx(label)))
                .child(div().flex_1().min_w_0().when(mono, |d| d.font_family("Menlo")).child(value))
        };
        let card = || v_flex().rounded(px(8.)).border_1().border_color(theme.border).text_sm().overflow_hidden();
        let header = h_flex()
            .gap_4()
            .items_center()
            .p_4()
            .rounded(px(10.))
            .border_1()
            .border_color(theme.primary.opacity(0.25))
            .bg(theme.primary.opacity(0.05))
            .child(img(self.avatar.clone()).flex_none().size(px(80.)).rounded_full().border_2().border_color(theme.primary.opacity(0.6)))
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(memex_client::BRAND_NAME))
                    .child(div().text_color(theme.primary).font_weight(FontWeight::MEDIUM).child(txf("版本 {0}", &[&APP_VERSION])))
                    .child(div().text_xs().text_color(muted).child(tx("开发版 · 仅本机使用"))),
            );
        let Some(info) = &self.version else {
            return v_flex().gap_4().max_w(px(640.)).child(header).child(div().text_sm().text_color(muted).child(tx("正在读取版本信息…")));
        };
        let b = &info.build;
        let app = card()
            .child(div().px_4().py_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(muted).child(tx("本应用")))
            .child(row("应用版本", APP_VERSION.to_string(), true))
            .child(row("构建编号", b.short_id().map(|s| s.to_string()).unwrap_or_else(unknown), true))
            .child(row("构建时间", b.built.clone().unwrap_or_else(unknown), false))
            .child(row("应用标识", b.bundle_id.clone().unwrap_or_else(|| tx("未打包（开发运行）").to_string()), true));
        let (connection, schema, features) = match &info.agent {
            AgentInfo::Fixture { schema_version, features } => (tx("合成数据（开发模式），未连接代理").to_string(), *schema_version, Some(features.clone())),
            AgentInfo::Agent { schema_version, features } => (tx("本机代理").to_string(), *schema_version, Some(features.clone())),
            AgentInfo::Unavailable(reason) => (txf("代理暂不可用：{0}", &[reason]), None, None),
        };
        let mut chips = h_flex().gap_1().flex_wrap();
        for f in features.iter().flatten() {
            chips = chips.child(div().px_2().rounded(px(4.)).bg(theme.muted).text_xs().font_family("Menlo").child(f.clone()));
        }
        let agent = card()
            .child(div().px_4().py_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(muted).child(tx("已连接的代理")))
            .child(row("连接", connection, false))
            .child(row("代理版本", info.agent_version.rows().0, true))
            .child(row("代理程序哈希", info.agent_version.rows().1, true))
            .child(row("协议版本", schema.map(|v| v.to_string()).unwrap_or_else(unknown), true))
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .items_start()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().w(px(120.)).flex_none().text_color(muted).child(tx("功能")))
                    .child(match &features {
                        Some(f) if !f.is_empty() => v_flex().flex_1().min_w_0().gap_1().child(txf("{0} 项", &[&f.len()])).child(chips).into_any_element(),
                        _ => div().child(unknown()).into_any_element(),
                    }),
            );
        v_flex().gap_4().max_w(px(640.)).child(header).child(app).child(agent)
    }

    /// The user picked a language: shown at once, applied and saved by the shell.
    pub fn choose_language(&mut self, language: LanguagePref, cx: &mut Context<Self>) {
        self.language = language;
        cx.emit(SettingsEvent::LanguageChanged(language));
        cx.notify();
    }

    /// Local appearance preferences apply immediately; never save model configuration.
    pub fn choose_reading_stats(&mut self, prefs: StatsPrefs, cx: &mut Context<Self>) {
        self.reading_stats = prefs.normalized();
        cx.emit(SettingsEvent::ReadingStatsChanged(self.reading_stats));
        cx.notify();
    }

    pub fn choose_note_font(&mut self, font: memex_client::NoteFont, cx: &mut Context<Self>) {
        self.note_font = font;
        cx.emit(SettingsEvent::NoteFontChanged(font));
        cx.notify();
    }

    pub fn choose_speaker_names(&mut self, names: memex_client::SpeakerNames, cx: &mut Context<Self>) {
        self.speaker_names = names;
        cx.emit(SettingsEvent::SpeakerNamesChanged(names));
        cx.notify();
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> Div {
        let p = self.reading_stats;
        let theme = cx.theme().clone();
        let mut method = h_flex().gap_1();
        for (i, (value, text)) in [(CountMethod::Mixed, stats_label("中文字 + 英文词", "Chinese characters + English words")), (CountMethod::Chars, stats_label("逐字符", "Every character"))].into_iter().enumerate() {
            method = method.child(Button::new(("count-method", i)).small().label(text)
                .when(p.method == value, |b| b.primary()).when(p.method != value, |b| b.outline())
                .on_click(cx.listener(move |s, _, _, cx| s.choose_reading_stats(StatsPrefs { method: value, ..s.reading_stats }, cx))));
        }
        let hint = if p.method == CountMethod::Mixed {
            stats_label("中文按字计（含标点），英文按词计（不含标点）", "Chinese characters include punctuation; English words exclude punctuation")
        } else { stats_label("所有非空白字符逐个计数", "Count every non-whitespace character") };
        let mut group = v_flex().gap_4().p_4().rounded(px(8.)).border_1().border_color(theme.border)
            .child(v_flex().gap_2().child(stats_label("字数统计方式", "Counting method")).child(div().text_xs().text_color(theme.muted_foreground).child(hint)).child(method))
            .child(h_flex().gap_3().child(v_flex().flex_1().gap_1().child(stats_label("统计 Markdown 标记", "Count Markdown markup"))
                .child(div().text_xs().text_color(theme.muted_foreground).child(stats_label("关闭时不计 #、**、链接地址、表格竖线等标记", "When off, exclude #, **, link destinations, table pipes and other markup"))))
                .child(Button::new("count-markup").small().label(if p.markup { stats_label("开启", "On") } else { stats_label("关闭", "Off") })
                    .when(p.markup, |b| b.primary()).when(!p.markup, |b| b.outline())
                    .on_click(cx.listener(|s, _, _, cx| s.choose_reading_stats(StatsPrefs { markup: !s.reading_stats.markup, ..s.reading_stats }, cx)))));
        for (zh, speeds, title, unit) in [
            (true, ZH_SPEEDS, stats_label("中文阅读速度", "Chinese reading speed"), stats_label("字/分钟", "characters/min")),
            (false, EN_SPEEDS, stats_label("英文阅读速度", "English reading speed"), stats_label("词/分钟", "words/min")),
        ] {
            let mut choices = h_flex().gap_1().flex_wrap();
            for &speed in speeds {
                let active = if zh { p.zh } else { p.en } == speed;
                choices = choices.child(Button::new((if zh { "count-zh" } else { "count-en" }, speed as usize)).small().label(speed.to_string())
                    .when(active, |b| b.primary()).when(!active, |b| b.outline())
                    .on_click(cx.listener(move |s, _, _, cx| {
                        let mut p = s.reading_stats;
                        if zh { p.zh = speed; } else { p.en = speed; }
                        s.choose_reading_stats(p, cx);
                    })));
            }
            group = group.child(v_flex().gap_2().child(format!("{title} · {unit}")).child(choices));
        }
        v_flex().gap_4().max_w(px(680.)).text_sm()
            .child(div().text_color(theme.muted_foreground).child(tx("外观（跟随系统 / 浅色 / 深色）在主窗口工具栏切换。")))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(stats_label("阅读统计", "Reading statistics")))
            .child(group)
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{}「{}」\n{}", stats_label("示例：", "Example: "), sample(), count_text(&sample(), p).label(p.method))))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tx("注释字体")))
            .child({
                let mut fonts = h_flex().gap_1();
                for (i, f) in memex_client::NoteFont::ALL.into_iter().enumerate() {
                    fonts = fonts.child(Button::new(("note-font", i)).small().label(f.label())
                        .when(self.note_font == f, |b| b.primary()).when(self.note_font != f, |b| b.outline())
                        .on_click(cx.listener(move |s, _, _, cx| s.choose_note_font(f, cx))));
                }
                v_flex().gap_3().p_4().rounded(px(8.)).border_1().border_color(theme.border)
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tx("注释缩小一号、与正文用分隔线隔开；这里选它的字体。")))
                    .child(fonts)
                    .child(div().pt_2().border_t_1().border_color(theme.border).text_color(theme.muted_foreground).font_family(self.note_font.family()).child(tx("① 这是一条注释的样子。")))
            })
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tx("说话人名字")))
            .child({
                let mut names = h_flex().gap_1();
                for (i, n) in memex_client::SpeakerNames::ALL.into_iter().enumerate() {
                    names = names.child(Button::new(("speaker-names", i)).small().label(n.label())
                        .when(self.speaker_names == n, |b| b.primary()).when(self.speaker_names != n, |b| b.outline())
                        .on_click(cx.listener(move |s, _, _, cx| s.choose_speaker_names(n, cx))));
                }
                v_flex().gap_3().p_4().rounded(px(8.)).border_1().border_color(theme.border)
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tx("「谁在说」气泡和译文段首的说话人标签按这里显示，只改显示、不改译文。原名只从已记录的逐段对照里两侧都明确标出的说话人得出；没有可靠对照时保留该侧原有的名字，并标「对照不可用」。")))
                    .child(names)
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child(tx("切换后立即生效，并保存在本机桌面设置中。")))
    }

    fn sync_placeholders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lang = memex_client::i18n::language();
        if self.placeholder_lang == lang {
            return;
        }
        self.placeholder_lang = lang;
        for (input, hint) in [(&self.key, KEY_HINT), (&self.model_label, MODEL_LABEL_HINT), (&self.model_id, MODEL_ID_HINT), (&self.provider_label, PROVIDER_LABEL_HINT), (&self.provider_base, PROVIDER_BASE_HINT)] {
            input.update(cx, |s, cx| s.set_placeholder(tx(hint), window, cx));
        }
    }

    /// General page: interface language as a segmented control, like the toolbar's view switch.
    fn render_general(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let mut segments = h_flex().rounded(px(7.)).bg(theme.muted).p(px(2.)).gap(px(2.));
        for (i, pref) in LanguagePref::ALL.into_iter().enumerate() {
            let active = self.language == pref;
            segments = segments.child(
                Button::new(("language", i))
                    .small()
                    .label(pref.label())
                    .when(active, |b| b.primary())
                    .when(!active, |b| b.ghost())
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_language(pref, cx))),
            );
        }
        let language = v_flex().rounded(px(8.)).border_1().border_color(theme.border).child(
            h_flex()
                .px_4()
                .py_3()
                .gap_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(tx("界面语言")))
                        .child(div().text_xs().text_color(muted).child(tx("切换后立即生效，并保存在本机桌面设置中。"))),
                )
                .child(segments),
        );
        v_flex()
            .gap_4()
            .child(language)
            .child(div().text_sm().text_color(muted).child(tx("数据与存储位置：等待后端提供只读路径接口后显示。")))
    }

    /// Reads settings, then each saved provider's credential state. Late answers are dropped.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.ticket += 1;
        let ticket = self.ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let settings = client.model_settings();
            let creds: Vec<_> = match &settings {
                Ok(s) => s.configuration.providers.iter().map(|p| (p.id.clone(), client.provider_credential(s.revision, &p.id))).collect(),
                Err(_) => vec![],
            };
            (settings, creds)
        });
        cx.spawn(async move |this, cx| {
            let (settings, creds) = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket {
                    return;
                }
                this.model.loaded(settings);
                for (id, c) in creds {
                    this.model.credential_loaded(&id, c);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        let Some((rev, cfg)) = self.model.start_save() else { return };
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.replace_model_settings(rev, cfg) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let saved = result.is_ok();
                this.model.finish_save(result);
                if saved {
                    this.reload(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn submit_key(&mut self, provider_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.key.read(cx).value().to_string();
        // Clear the field first: the value exists only in this request.
        self.key.update(cx, |k, cx| k.set_value("", window, cx));
        self.key_for = None;
        let (Some(rev), Some(gen)) = (self.model.revision(), self.model.credential(&provider_id).map(|c| c.generation)) else { return };
        if value.is_empty() {
            return;
        }
        let client = self.client.clone();
        let id = provider_id.clone();
        let task = cx.background_executor().spawn(async move { client.replace_provider_credential(rev, &id, gen, Secret::new(value)) });
        self.after_credential(provider_id, task, cx);
    }

    fn clear_key(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let (Some(rev), Some(gen)) = (self.model.revision(), self.model.credential(&provider_id).map(|c| c.generation)) else { return };
        let client = self.client.clone();
        let id = provider_id.clone();
        let task = cx.background_executor().spawn(async move { client.clear_provider_credential(rev, &id, gen) });
        self.after_credential(provider_id, task, cx);
    }

    fn after_credential(&mut self, provider_id: String, task: Task<Result<memex_client::ProviderCredential, memex_client::ClientError>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                // An unknown outcome is read back, never resent.
                if result.is_err() {
                    this.model.credential_loaded(&provider_id, result);
                    this.reload(cx);
                } else {
                    this.model.credential_loaded(&provider_id, result);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn discover(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let Some(rev) = self.model.revision() else { return };
        let authorization = match self.model.credential(&provider_id) {
            Some(c) if c.state == CredentialState::Saved => DiscoverAuthorization::SavedKey { expected_generation: c.generation },
            _ => DiscoverAuthorization::Credentialless,
        };
        let client = self.client.clone();
        let id = provider_id.clone();
        let task = cx.background_executor().spawn(async move { client.discover_models(rev, &id, authorization) });
        self.model.set_message(Some(tx("正在获取模型列表…（只读取列表，不发送任何正文）").into()));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.model.set_message(None);
                this.model.discovery_loaded(&provider_id, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn submit_model(&mut self, provider_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.model_label.read(cx).value().to_string();
        let id = self.model_id.read(cx).value().to_string();
        match self.model.add_model(&provider_id, &label, &id) {
            Ok(_) => {
                self.model_for = None;
                self.model_label.update(cx, |s, cx| s.set_value("", window, cx));
                self.model_id.update(cx, |s, cx| s.set_value("", window, cx));
                self.model.set_message(None);
            }
            Err(e) => self.model.set_message(Some(e)),
        }
        cx.notify();
    }

    fn submit_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.provider_label.read(cx).value().to_string();
        let base = self.provider_base.read(cx).value().to_string();
        match self.model.add_provider(&label, &base) {
            Ok(()) => {
                self.adding_provider = false;
                self.provider_label.update(cx, |s, cx| s.set_value("", window, cx));
                self.provider_base.update(cx, |s, cx| s.set_value("", window, cx));
                self.model.set_message(None);
            }
            Err(e) => self.model.set_message(Some(e)),
        }
        cx.notify();
    }

    /// Scrolls the page body to its last controls (add model / add provider).
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Whether the body is scrolled to its end, once laid out.
    pub fn at_end(&self) -> bool {
        let max = self.scroll.max_offset().y;
        max <= px(1.) || (-self.scroll.offset().y) >= max - px(1.)
    }

    /// Development intents for scripted review (same effects as the controls).
    pub fn dev_add_provider(&mut self, label: &str, base: &str, models: &[(&str, &str)], cx: &mut Context<Self>) {
        if self.model.add_provider(label, base).is_ok() {
            let id = self.model.draft().providers.last().unwrap().id.clone();
            for (l, m) in models {
                let _ = self.model.add_model(&id, l, m);
            }
        }
        cx.notify();
    }

    fn purpose_menu(&self, purpose: Purpose, cx: &mut Context<Self>) -> AnyElement {
        let current = self.model.selection(purpose).map(|m| self.card_label(&m.id)).unwrap_or_else(|| tx("未配置").into());
        let options: Vec<(String, String)> = self.model.draft().models.iter().map(|m| (m.id.clone(), self.card_label(&m.id))).collect();
        let selected = self.model.selection(purpose).map(|m| m.id.clone());
        let handle = cx.entity();
        Button::new(SharedString::from(format!("purpose-{purpose:?}")))
            .outline()
            .small()
            .w(px(240.))
            .label(current)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.item({
                    let h = handle.clone();
                    PopupMenuItem::new(tx("未配置")).checked(selected.is_none()).on_click(move |_, _, cx| h.update(cx, |s, cx| { s.model.select(purpose, None); cx.notify(); }))
                });
                for (id, label) in &options {
                    let h = handle.clone();
                    let id2 = id.clone();
                    menu = menu.item(PopupMenuItem::new(label.clone()).checked(selected.as_ref() == Some(id)).on_click(move |_, _, cx| {
                        h.update(cx, |s, cx| {
                            s.model.select(purpose, Some(id2.clone()));
                            cx.notify();
                        })
                    }));
                }
                menu
            })
            .into_any_element()
    }

    /// Effort is stored on the route, so it is only editable once a model is chosen.
    /// A value from a newer agent is shown as-is and kept unless the user changes it.
    fn effort_menu(&self, purpose: Purpose, cx: &mut Context<Self>) -> AnyElement {
        let id = SharedString::from(format!("effort-{purpose:?}"));
        if self.model.selection(purpose).is_none() {
            return Button::new(id).ghost().xsmall().label("—").disabled(true).into_any_element();
        }
        let current = self.model.reasoning_effort(purpose).map(str::to_string);
        let handle = cx.entity();
        Button::new(id)
            .ghost()
            .xsmall()
            .label(current.as_deref().map(effort_label).unwrap_or_else(|| tx("未指定").into()))
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for value in std::iter::once(None).chain(REASONING_EFFORTS.iter().map(|e| Some(*e))) {
                    let h = handle.clone();
                    let label = value.map(effort_label).unwrap_or_else(|| tx("未指定").into());
                    menu = menu.item(PopupMenuItem::new(label).checked(current.as_deref() == value).on_click(move |_, _, cx| {
                        h.update(cx, |s, cx| {
                            s.model.set_reasoning_effort(purpose, value);
                            cx.notify();
                        })
                    }));
                }
                menu
            })
            .into_any_element()
    }

    fn card_label(&self, card_id: &str) -> String {
        let d = self.model.draft();
        let Some(m) = d.models.iter().find(|m| m.id == card_id) else { return card_id.into() };
        let p = d.providers.iter().find(|p| p.id == m.provider_id).map(|p| p.label.as_str()).unwrap_or("?");
        txf("{0}（{1}）", &[&m.label, &p])
    }

    fn render_models(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let pending = |text: &'static str| div().px_2().rounded(px(4.)).bg(theme.primary.opacity(0.12)).text_color(theme.primary).text_xs().child(text);
        let mut page = v_flex().gap_4();
        // English purpose names ("Batch classification") need a wider column.
        let label_w = if memex_client::i18n::language() == Lang::En { 150. } else { 110. };
        for group in [Group::Understanding, Group::Translation, Group::Decision] {
            let (title, desc) = group.title();
            let mut card = v_flex().rounded(px(8.)).border_1().border_color(theme.border).child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().flex_1().text_xs().text_color(muted).child(desc))
                    .child(div().w(px(120.)).text_xs().text_color(muted).child(tx("推理强度"))),
            );
            for row in ROWS.iter().filter(|r| r.group == group) {
                let (control, effort): (AnyElement, AnyElement) = match row.purpose {
                    Some(p) => (self.purpose_menu(p, cx), self.effort_menu(p, cx)),
                    None => (
                        Button::new(SharedString::from(format!("off-{}", row.label))).outline().small().w(px(240.)).label(tx("暂不可选")).disabled(true).into_any_element(),
                        Button::new(SharedString::from(format!("effort-{}", row.label))).ghost().xsmall().label("—").disabled(true).into_any_element(),
                    ),
                };
                card = card.child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_3()
                        .border_b_1()
                        .border_color(theme.border.opacity(0.5))
                        .child(div().w(px(label_w)).flex_shrink_0().text_sm().when(row.purpose.is_none(), |d| d.text_color(muted)).child(tx(row.label)))
                        .child(h_flex().flex_1().gap_2().text_xs().text_color(muted).when(row.purpose.is_none(), |d| d.child(pending(tx("待后端支持")))).child(tx(row.hint)))
                        .child(control)
                        .child(div().w(px(120.)).child(effort)),
                );
            }
            page = page.child(card);
        }
        page = page.child(h_flex().gap_2().text_xs().text_color(muted).child(Icon::new(IconName::Info).xsmall()).child(tx("这里只保存模型与推理强度，不等于已能生成；标“待后端支持”的用途尚未开放。")));
        page.child(self.render_providers(cx))
    }

    fn render_providers(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let mut section = v_flex().gap_3().child(div().font_weight(FontWeight::SEMIBOLD).child(tx("服务提供方")));
        let providers = self.model.draft().providers.clone();
        for p in providers {
            let saved = self.model.provider_is_saved(&p.id);
            let local = p.api_base.starts_with("http://127.0.0.1") || p.api_base.starts_with("http://[::1]");
            let cred = self.model.credential(&p.id).cloned();
            let pid = p.id.clone();
            let mut card = v_flex()
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .p_3()
                .gap_2()
                .text_sm()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::Inbox).small())
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(p.label.clone()))
                        .child(div().flex_1())
                        .when(local, |d| d.child(div().px_2().rounded_full().border_1().border_color(theme.border).text_xs().text_color(muted).child(tx("本机入口，可能转发到远端服务"))))
                        .child(Button::new(SharedString::from(format!("rm-{pid}"))).ghost().xsmall().label(tx("删除")).on_click(cx.listener({
                            let pid = pid.clone();
                            move |this, _, _, cx| {
                                if let Err(e) = this.model.remove_provider(&pid) {
                                    this.model.set_message(Some(e));
                                }
                                cx.notify();
                            }
                        }))),
                )
                .child(h_flex().gap_3().child(div().w(px(80.)).text_color(muted).child(tx("地址"))).child(p.api_base.clone()));
            // Credentials: state and masked suffix only; replace/clear take effect immediately.
            let key_row = h_flex()
                .gap_3()
                .child(div().w(px(80.)).text_color(muted).child(tx("密钥")))
                .child(div().flex_1().child(if saved { self.model.credential_label(&p.id) } else { tx("保存设置后才能管理密钥").into() }))
                .when(saved, |d| {
                    d.child(Button::new(SharedString::from(format!("key-{pid}"))).outline().xsmall().label(if cred.as_ref().is_some_and(|c| c.state == CredentialState::Missing) { tx("输入密钥") } else { tx("更换") }).on_click(cx.listener({
                        let pid = pid.clone();
                        move |this, _, _, cx| {
                            this.key_for = Some(pid.clone());
                            cx.notify();
                        }
                    })))
                    .when(cred.as_ref().is_some_and(|c| c.state != CredentialState::Missing), |d| {
                        d.child(Button::new(SharedString::from(format!("clear-{pid}"))).ghost().xsmall().label(tx("清除")).on_click(cx.listener({
                            let pid = pid.clone();
                            move |this, _, _, cx| this.clear_key(pid.clone(), cx)
                        })))
                    })
                });
            card = card.child(key_row);
            if self.key_for.as_deref() == Some(p.id.as_str()) {
                card = card.child(
                    h_flex()
                        .gap_2()
                        .pl(px(92.))
                        .child(div().flex_1().child(Input::new(&self.key).small().mask_toggle()))
                        .child(Button::new("key-submit").primary().xsmall().label(tx("保存密钥")).on_click(cx.listener({
                            let pid = pid.clone();
                            move |this, _, window, cx| this.submit_key(pid.clone(), window, cx)
                        })))
                        .child(Button::new("key-cancel").ghost().xsmall().label(tx("取消")).on_click(cx.listener(|this, _, window, cx| {
                            this.key.update(cx, |k, cx| k.set_value("", window, cx));
                            this.key_for = None;
                            cx.notify();
                        }))),
                );
            }
            let discovered = self.model.discovered(&p.id).map(|d| d.model_ids.len());
            card = card.child(
                h_flex()
                    .gap_3()
                    .child(div().w(px(80.)).text_color(muted).child(tx("模型列表")))
                    .child(Button::new(SharedString::from(format!("discover-{pid}"))).outline().xsmall().label(tx("获取模型列表")).disabled(!saved).on_click(cx.listener({
                        let pid = pid.clone();
                        move |this, _, _, cx| this.discover(pid.clone(), cx)
                    })))
                    .child(div().text_xs().text_color(muted).child(match discovered {
                        Some(n) => txf("上次获取：{0} 个模型 · 仅表示对方列出，未验证可用", &[&n]),
                        None => tx("尚未获取 · 只在你点击时读取列表").into(),
                    })),
            );
            let models: Vec<_> = self.model.draft().models.iter().filter(|m| m.provider_id == p.id).cloned().collect();
            let mut table = v_flex().ml(px(92.)).rounded(px(6.)).border_1().border_color(theme.border).text_xs().child(
                h_flex().px_2().py_1().bg(theme.muted).text_color(muted).child(div().w(px(160.)).child(tx("显示名"))).child(div().w(px(200.)).child(tx("模型 ID"))).child(div().flex_1().child(tx("来源"))),
            );
            for m in &models {
                let source = self.model.source(m);
                let mid = m.id.clone();
                table = table.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(theme.border.opacity(0.5))
                        .child(div().w(px(160.)).truncate().child(m.label.clone()))
                        .child(div().w(px(200.)).truncate().child(m.model_id.clone()))
                        .child(div().flex_1().child(match source {
                            ModelSource::Discovered => div().child(tx("列表获取")),
                            ModelSource::ManualUnverified => div().px_1().rounded(px(4.)).bg(theme.primary.opacity(0.1)).text_color(theme.primary).child(tx("手动添加 · 未验证")),
                        }))
                        .child(Button::new(SharedString::from(format!("rmm-{mid}"))).ghost().xsmall().icon(IconName::Close).on_click(cx.listener(move |this, _, _, cx| {
                            let cleared = this.model.remove_model(&mid);
                            if !cleared.is_empty() {
                                this.model.set_message(Some(tx("已同时清除使用该模型的用途").into()));
                            }
                            cx.notify();
                        }))),
                );
            }
            card = card.child(table);
            if self.model_for.as_deref() == Some(p.id.as_str()) {
                card = card.child(
                    h_flex()
                        .gap_2()
                        .ml(px(92.))
                        .child(div().w(px(160.)).child(Input::new(&self.model_label).small()))
                        .child(div().flex_1().child(Input::new(&self.model_id).small()))
                        .child(Button::new("model-submit").primary().xsmall().label(tx("添加")).on_click(cx.listener({
                            let pid = pid.clone();
                            move |this, _, window, cx| this.submit_model(pid.clone(), window, cx)
                        }))),
                );
            } else {
                card = card.child(div().ml(px(92.)).child(Button::new(SharedString::from(format!("addm-{pid}"))).outline().xsmall().label(tx("手动添加模型")).on_click(cx.listener({
                    let pid = pid.clone();
                    move |this, _, _, cx| {
                        this.model_for = Some(pid.clone());
                        cx.notify();
                    }
                }))));
            }
            section = section.child(card);
        }
        if self.adding_provider {
            section = section.child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(180.)).child(Input::new(&self.provider_label).small()))
                    .child(div().flex_1().child(Input::new(&self.provider_base).small()))
                    .child(Button::new("provider-submit").primary().xsmall().label(tx("添加")).on_click(cx.listener(|this, _, window, cx| this.submit_provider(window, cx)))),
            );
        } else {
            section = section.child(div().child(Button::new("add-provider").outline().small().label(tx("添加服务提供方")).on_click(cx.listener(|this, _, _, cx| {
                this.adding_provider = true;
                cx.notify();
            }))));
        }
        section
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_placeholders(window, cx);
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let status = self.model.status();
        let mut nav = v_flex().w(px(180.)).h_full().p_2().gap_1().bg(theme.sidebar).border_r_1().border_color(theme.border);
        for (i, (page, label, icon)) in [(Page::General, "通用", IconName::Settings), (Page::Appearance, "外观", IconName::Sun), (Page::Models, "模型", IconName::Bot), (Page::Version, "版本", IconName::Info), (Page::Changelog, "更新日志", IconName::Star)].into_iter().enumerate() {
            let active = self.page == page;
            nav = nav.child(
                h_flex()
                    .id(("nav", i))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .text_sm()
                    .when(active, |d| d.bg(theme.primary.opacity(0.14)).text_color(theme.primary))
                    .when(!active, |d| d.hover(|d| d.bg(theme.muted)))
                    .child(Icon::new(icon).small())
                    .child(tx(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    })),
            );
        }
        let body: AnyElement = match (&self.page, &status) {
            (Page::Models, Status::Loading) => div().text_sm().text_color(muted).child(tx("正在读取设置…")).into_any_element(),
            (Page::Models, Status::Unavailable(reason)) => v_flex().gap_2().text_sm().child(tx("模型设置暂不可用")).child(div().text_color(muted).child(relabel(reason))).into_any_element(),
            (Page::Models, _) => self.render_models(cx).into_any_element(),
            (Page::General, _) => self.render_general(cx).into_any_element(),
            (Page::Appearance, _) => self.render_appearance(cx).into_any_element(),
            (Page::Version, _) => self.render_version(cx).into_any_element(),
            (Page::Changelog, _) => self.render_changelog(cx).into_any_element(),
        };
        let dirty = self.model.is_dirty();
        let conflict = status == Status::Conflict;
        let footer = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(muted)
            .child(div().flex_1().child(match self.model.revision() {
                Some(r) => format!("{}{}", txf("配置版本 {0}", &[&r]), if dirty { tx(" · 未保存的修改") } else { "" }),
                None => String::new(),
            }))
            .when(conflict, |d| d.child(Button::new("reload").outline().small().label(tx("重新载入")).on_click(cx.listener(|this, _, _, cx| this.reload(cx)))))
            .child(Button::new("discard").outline().small().label(tx("放弃修改")).disabled(!dirty).on_click(cx.listener(|this, _, _, cx| {
                this.model.discard();
                cx.notify();
            })))
            .child(Button::new("save-settings").primary().small().label(if status == Status::Saving { tx("保存中…") } else { tx("保存") }).disabled(!dirty || conflict || status == Status::Saving).on_click(cx.listener(|this, _, _, cx| this.save(cx))));
        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when_some(self.model.message().map(relabel), |d, m| {
                        d.child(h_flex().gap_2().px_4().py_1().bg(if conflict { theme.warning.opacity(0.12) } else { theme.accent }).text_sm().child(Icon::new(IconName::Info).small()).child(m))
                    })
                    .child(div().id("settings-body").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).p_4().child(body))
                    .when(self.page == Page::Models, |d| d.child(footer)),
            )
    }
}
