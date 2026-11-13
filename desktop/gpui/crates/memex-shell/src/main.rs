//! MemeX desktop shell: window, toolbar, reading/classic layouts, docking,
//! appearance and module wiring. Modules talk only through their public events;
//! the shell owns navigation (selection guard) and layout preferences.

mod prefs;
mod theme;
#[cfg(feature = "dev-snapshot")]
mod devdrive;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, h_resizable, resizable_panel, switch::Switch, v_flex, v_resizable, ActiveTheme as _, Icon, IconName, Sizable as _,
    ResizableState, TitleBar,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use memex_chat::view::{Chat, ChatEvent};
use memex_client::{fixture::{FixtureBackend, FixtureControls}, i18n, relabel, tx, txf, AgentBackend, Client, Lang, LanguagePref};
use memex_library::{
    import::DropSpot,
    import_view::{Importer, ImporterEvent},
    model::LoadState,
    trash::TrashView,
    view::{Library, LibraryEvent, LibraryList, LibrarySidebar, LibraryTable},
};
use memex_metadata::view::{ItemFacts, Layout, Metadata, MetadataEvent};
use memex_reader::view::{Reader, ReaderEvent};
use prefs::{Appearance, Connection, Prefs, ReaderMode, Startup, ViewMode};

actions!(memex, [ShowReading, ShowClassic, ToggleSidebar, ToggleConversation, ToggleReadingFont, Refresh, OpenSettings, Quit]);

const GUARD_NOTICE: &str = "请先保存或取消当前条目的修改";

struct Shell {
    fixture: Option<FixtureBackend>,
    /// The desktop location/settings could not be used; nothing is connected.
    config_error: Option<String>,
    /// Where preferences are saved; None when the settings file was unusable.
    prefs_path: Option<std::path::PathBuf>,
    client: Client,
    settings: Option<(AnyWindowHandle, Entity<memex_settings::view::Settings>)>,
    prefs: Prefs,
    system_dark: bool,
    appearance_menu: bool,
    /// macOS preferred language, used when the preference is `system`.
    system_lang: Lang,
    /// Interface language of the last frame; a change re-localizes stored text.
    rendered_lang: Lang,
    /// Stored as a Chinese key or in the language it was made in; shown via `relabel`.
    notice: Option<String>,
    library: Entity<Library>,
    sidebar: Entity<LibrarySidebar>,
    table: Entity<LibraryTable>,
    list: Entity<LibraryList>,
    trash: Entity<TrashView>,
    metadata: Entity<Metadata>,
    reader: Entity<Reader>,
    chat: Entity<Chat>,
    /// Files dragged in from Finder: the import tray and its backend.
    importer: Entity<Importer>,
    /// The synthetic import backend (fixture mode; development switches).
    #[cfg_attr(not(any(test, feature = "dev-snapshot")), allow(dead_code))]
    import_fixture: Option<memex_client::fixture::FixtureImport>,
    /// A file drag from Finder is over the window: how many paths it carries.
    file_drag: Option<usize>,
    main_split: Entity<ResizableState>,
    right_split: Entity<ResizableState>,
    center_split: Entity<ResizableState>,
    /// Window y of the conversation pane's top edge (the divider), last frame.
    conversation_top: std::rc::Rc<std::cell::Cell<Pixels>>,
    /// Split height the saved reader share was last applied to.
    center_applied: Option<Pixels>,
    focus: FocusHandle,
    /// The chat asked for settings; opened on the next frame (needs the window).
    pending_settings: bool,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Errors found while resolving the startup are worded in the system language
        // (a broken configuration has no usable preference of its own).
        let system_lang = prefs::system_language();
        i18n::set_language(system_lang);
        Self::with_startup(Startup::from_environment(), system_lang, window, cx)
    }

    /// The shell for a resolved startup (tests pass their own prefs path and the fixture).
    fn with_startup(startup: Startup, system_lang: Lang, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Startup { prefs, prefs_path, connection } = startup;
        i18n::set_language(prefs.language.resolve(system_lang));
        memex_client::set_reading_font(prefs.reading_font);
        memex_client::set_note_font(prefs.note_font);
        memex_client::set_speaker_names(prefs.speaker_names);
        set_app_menus(cx);
        let (client, fixture, config_error) = match connection {
            Ok(Connection::Agent(socket)) => (Client::new(AgentBackend::new(socket)), None, None),
            Ok(Connection::Fixture) => {
                let count = std::env::var("MEMEX_FIXTURE_ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(128);
                let fixture = FixtureBackend::new(count);
                (Client::new(fixture.clone()), Some(fixture), None)
            }
            // No fixture and no default daemon: every request reports unavailable.
            Err(message) => (Client::new(Unconfigured), None, Some(message)),
        };
        let library = cx.new(|cx| Library::new(client.clone(), window, cx));
        // Pending 删除 / 恢复 request ids live next to the desktop settings.
        library.update(cx, |l, _| l.set_request_store(prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("group-delete-requests.json"))));
        let sidebar = cx.new(|cx| LibrarySidebar::new(library.clone(), cx));
        let table = cx.new(|cx| LibraryTable::new(library.clone(), window, cx));
        let list = cx.new(|cx| LibraryList::new(library.clone(), cx));
        let trash = cx.new(|cx| TrashView::new(library.clone(), cx));
        let metadata = cx.new(|cx| Metadata::new(client.clone(), window, cx));
        let reader = cx.new(|cx| Reader::new(client.clone(), cx));
        let reader_mode = prefs.reader_mode.to_reader();
        reader.update(cx, |r, cx| {
            r.model.set_stats_prefs(prefs.reading_stats);
            r.set_reading_size(prefs.reading_size, cx);
            r.restore_mode(reader_mode, cx);
            r.restore_book_parallel(prefs.book_parallel, cx);
            r.restore_pass_timing(prefs.pass_timing);
            r.pass_view_shown(prefs.view == ViewMode::Reading, cx);
            r.set_item_styles(prefs.item_styles.clone());
            // Pending 整理本章段落 request ids live next to the desktop settings.
            r.set_repair_store(prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("reader-repair-requests.json")));
        });
        let shell = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.allow_close(cx)).unwrap_or(true)
        });
        let shell = cx.entity().downgrade();
        let app: &mut App = cx;
        app.on_action(move |_: &Quit, cx: &mut App| {
            if shell.update(cx, |this, cx| this.allow_close(cx)).unwrap_or(true) { cx.quit(); }
        });
        let chat = cx.new(|cx| Chat::new(client.clone(), window, cx));
        // File import: the synthetic backend next to the fixture, else the agent's
        // placeholder (sends nothing until B's first-intake command exists).
        let (import_backend, import_fixture): (std::sync::Arc<dyn memex_client::file_import::ImportBackend>, _) = match &fixture {
            Some(f) => {
                let synthetic = memex_client::fixture::FixtureImport::new(Some(f.clone()));
                (std::sync::Arc::new(synthetic.clone()), Some(synthetic))
            }
            None => (std::sync::Arc::new(memex_client::file_import::AgentImport::new(client.clone())), None),
        };
        let importer = cx.new(|cx| Importer::new(import_backend, Some(client.clone()), cx));
        metadata.update(cx, |m, _| m.layout = if prefs.view == ViewMode::Classic { Layout::Inspector } else { Layout::Compact });
        let include_subfolders = prefs.include_subfolders;
        library.update(cx, |l, cx| l.set_include_descendants(include_subfolders, cx));

        let subscriptions = vec![
            cx.observe_window_activation(window, |this, window, cx| {
                // 阅读计时 accrues only while the window is in front.
                let active = window.is_window_active();
                this.reader.update(cx, |r, cx| r.pass_foreground(active, cx));
                if window.is_window_active() {
                    this.library.update(cx, |l, cx| l.refresh_if_changed(cx));
                    // Settings (chat or translation models) may have changed meanwhile.
                    this.refresh_capabilities(cx);
                }
            }),
            cx.subscribe(&importer, |this, _, event: &ImporterEvent, cx| match event {
                ImporterEvent::Open(id) => this.select(id.clone(), true, cx),
                ImporterEvent::Imported => this.library.update(cx, |l, cx| l.reload(cx)),
            }),
            cx.subscribe_in(&library, window, |this, _, event: &LibraryEvent, window, cx| match event {
                LibraryEvent::SelectionRequested(id) => this.select(id.clone(), false, cx),
                LibraryEvent::OpenRequested(id) => this.select(id.clone(), true, cx),
                LibraryEvent::RenameRequested(id) => {
                    if this.metadata.read(cx).blocks_navigation(cx) || this.reader.read(cx).blocks_navigation(cx) {
                        this.notice = Some(GUARD_NOTICE.into());
                        cx.notify();
                    } else {
                        this.library.update(cx, |l, cx| l.start_item_rename(id, window, cx));
                    }
                }
                LibraryEvent::ItemRenamed(id) => {
                    if this.metadata.read(cx).item_id() == Some(id.as_str()) {
                        this.metadata.update(cx, |m, cx| m.reload(cx));
                        this.reader.update(cx, |r, cx| r.reload(cx));
                    }
                }
                LibraryEvent::GroupsEdited => {}
                LibraryEvent::FilesDropped { paths, spot } => this.import_files(paths.clone(), spot.clone(), cx),
                LibraryEvent::FormatBatch { group_id, name } => {
                    let library = this.library.read(cx);
                    let include = library.model.include_descendants();
                    let titles = (0..library.model.len()).filter_map(|i| library.model.row(i)).map(|r| (r.item_id.clone(), r.display_title())).collect();
                    let (group_id, name) = (group_id.clone(), name.clone());
                    this.set_view(ViewMode::Reading, cx);
                    this.reader.update(cx, |r, cx| r.open_format_batch(&group_id, &name, include, titles, cx));
                }
            }),
            cx.subscribe(&metadata, |this, _, event: &MetadataEvent, cx| match event {
                MetadataEvent::Saved { .. } => {
                    this.library.update(cx, |l, cx| l.reload(cx));
                    this.reader.update(cx, |r, cx| r.reload(cx));
                }
                // 生成 in the side panel: the reader asks and follows, as its 纪要 button does.
                MetadataEvent::GenerateMinutes => this.reader.update(cx, |r, cx| r.request_minutes(cx)),
                MetadataEvent::ReadMinutes => {
                    this.set_view(ViewMode::Reading, cx);
                    this.reader.update(cx, |r, cx| r.open_minutes(cx));
                }
            }),
            cx.observe(&metadata, |this, metadata, cx| {
                if this.notice.as_deref() == Some(GUARD_NOTICE) && !metadata.read(cx).blocks_navigation(cx) {
                    this.notice = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&reader, window, |this, _, event: &ReaderEvent, window, cx| match event {
                ReaderEvent::Saved => {
                    this.library.update(cx, |l, cx| l.reload(cx));
                    if !this.metadata.read(cx).blocks_navigation(cx) { this.metadata.update(cx, |m, cx| m.reload(cx)); }
                }
                ReaderEvent::Activated(Some(id)) => this.select(id.clone(), false, cx),
                ReaderEvent::Activated(None) => {}
                ReaderEvent::ModeChanged(mode) => {
                    this.prefs.reader_mode = ReaderMode::from_reader(*mode);
                    this.save_prefs();
                }
                ReaderEvent::OpenSettings => this.open_settings(window, cx),
                // Titles in the side panel use the same typeface.
                ReaderEvent::FontChanged(font) => {
                    this.prefs.reading_font = *font;
                    this.save_prefs();
                    cx.refresh_windows();
                }
                ReaderEvent::ItemStyleChanged(id, style) => {
                    if style.is_default() { this.prefs.item_styles.remove(id); } else { this.prefs.item_styles.insert(id.clone(), *style); }
                    this.save_prefs();
                }
                ReaderEvent::SizeChanged(size) => {
                    this.prefs.reading_size = *size;
                    this.save_prefs();
                }
                ReaderEvent::BookParallelChanged(parallel) => {
                    this.prefs.book_parallel = *parallel;
                    this.save_prefs();
                }
                ReaderEvent::PassTimingChanged(on) => {
                    this.prefs.pass_timing = *on;
                    this.save_prefs();
                }
                // The metadata panel shows the translation state too; keep any draft.
                ReaderEvent::TranslationReady => {
                    if !this.metadata.read(cx).blocks_navigation(cx) {
                        this.metadata.update(cx, |m, cx| m.reload(cx));
                    }
                }
                // The quote goes into the composer; only 发送 asks.
                ReaderEvent::Ask(quote) => {
                    this.set_conversation_collapsed(false, cx);
                    let composer = this.chat.read(cx).composer().clone();
                    let text = format!("{}{}", quote, composer.read(cx).value());
                    composer.update(cx, |c, cx| { c.set_value(text, window, cx); c.focus(window, cx); });
                }
            }),
            cx.subscribe(&chat, |this, _, event: &ChatEvent, cx| match event {
                ChatEvent::CitationClicked(c) => this.reader.update(cx, |r, cx| r.jump(c.clone(), cx)),
                ChatEvent::ConversationChanged => cx.notify(),
                ChatEvent::OpenSettings => {
                    this.pending_settings = true;
                    cx.notify();
                }
                // A new question shows its answer: a folded conversation unfolds.
                ChatEvent::Sent => this.set_conversation_collapsed(false, cx),
                ChatEvent::ToggleCollapsed => this.toggle_conversation(cx),
            }),
            // Chat follows the reader's loaded revision.
            cx.observe(&reader, |this, reader, cx| {
                let detail = reader.read(cx).model.detail().cloned();
                let fixture = this.fixture.is_some();
                this.chat.update(cx, |chat, cx| match detail {
                    Some(d) if chat_key(chat) != Some((d.canonical.item_id.clone(), d.canonical.revision_id.clone())) => {
                        let sample = fixture && d.canonical.item_id.ends_with(['0', '3']);
                        chat.open(&d.canonical.item_id, &d.canonical.revision_id, &d.canonical.markdown, sample, cx);
                    }
                    None if chat.model.has_conversation() => chat.close(cx),
                    _ => {}
                });
                cx.notify();
            }),
            cx.observe(&library, |this, library, cx| {
                // First load: select the first item once rows exist.
                let l = library.read(cx);
                if l.model.selected().is_none() {
                    if let Some(row) = l.model.row(0).cloned() {
                        this.select(row.item_id, false, cx);
                    }
                }
                cx.notify();
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                this.system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
                this.apply_appearance(cx);
            }),
        ];
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        let mut this = Self {
            fixture,
            notice: config_error.as_deref().map(config_notice),
            config_error,
            prefs_path,
            client: client.clone(),
            settings: None,
            prefs,
            system_dark,
            appearance_menu: false,
            system_lang,
            rendered_lang: i18n::language(),
            library,
            sidebar,
            table,
            list,
            trash,
            metadata,
            reader,
            chat,
            importer,
            import_fixture,
            file_drag: None,
            main_split: cx.new(|_| ResizableState::default()),
            right_split: cx.new(|_| ResizableState::default()),
            center_split: cx.new(|_| ResizableState::default()),
            conversation_top: Default::default(),
            center_applied: None,
            focus: cx.focus_handle(),
            pending_settings: false,
            _subscriptions: subscriptions,
        };
        this.apply_appearance(cx);
        window.focus(&this.focus, cx);
        #[cfg(feature = "dev-snapshot")]
        devdrive::start(window, cx);
        this
    }

    /// Settings live in their own window; reopening focuses the existing one.
    /// Opens on the display currently showing the main window, so a MemeX
    /// window on an external screen never pops its settings onto the built-in one.
    fn open_settings(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some((handle, _)) = &self.settings {
            // An unattended development run never activates a window.
            let unattended = dev_unattended();
            if handle.update(cx, |_, window, _| if !unattended { window.activate_window() }).is_ok() {
                return;
            }
        }
        let client = self.client.clone();
        let language = self.prefs.language;
        let display = window.display(cx).map(|d| d.id()).or_else(|| dev_display(cx));
        let mut options = TitleBar::window_options();
        options.display_id = display;
        // Development walks run unattended and must not steal focus; without their
        // spare screen they are not shown at all (frames are rendered off screen).
        options.focus = !dev_unattended();
        options.show = !dev_unattended() || dev_display(cx).is_some();
        options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(display, size(px(1100.), px(820.)), cx)));
        options.window_min_size = Some(size(px(860.), px(560.)));
        if let Ok(opened) = gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| memex_settings::view::Settings::new(client, language, window, cx))) {
            opened.1.update(cx, |s, cx| {
                s.reading_stats = self.prefs.reading_stats;
                s.note_font = self.prefs.note_font;
                s.speaker_names = self.prefs.speaker_names;
                cx.notify();
            });
            let sub = cx.subscribe(&opened.1, |this, _, event: &memex_settings::view::SettingsEvent, cx| match event {
                memex_settings::view::SettingsEvent::LanguageChanged(pref) => this.set_language(*pref, cx),
                memex_settings::view::SettingsEvent::ReadingStatsChanged(pref) => this.set_reading_stats(*pref, cx),
                memex_settings::view::SettingsEvent::NoteFontChanged(font) => this.set_note_font(*font, cx),
                memex_settings::view::SettingsEvent::SpeakerNamesChanged(names) => this.set_speaker_names(*names, cx),
            });
            self._subscriptions.push(sub);
            self.settings = Some(opened);
        }
    }

    pub(crate) fn set_note_font(&mut self, font: memex_client::NoteFont, cx: &mut Context<Self>) {
        self.prefs.note_font = font;
        memex_client::set_note_font(font);
        self.save_prefs();
        self.reader.update(cx, |r, cx| r.remeasure(cx));
        cx.notify();
    }

    pub(crate) fn set_speaker_names(&mut self, names: memex_client::SpeakerNames, cx: &mut Context<Self>) {
        self.prefs.speaker_names = names;
        memex_client::set_speaker_names(names);
        self.save_prefs();
        self.reader.update(cx, |r, cx| r.remeasure(cx));
        cx.notify();
    }

    fn set_reading_stats(&mut self, pref: memex_client::reading_stats::StatsPrefs, cx: &mut Context<Self>) {
        self.prefs.reading_stats = pref.normalized();
        self.save_prefs();
        self.reader.update(cx, |r, cx| {
            r.model.set_stats_prefs(self.prefs.reading_stats);
            cx.notify();
        });
        cx.notify();
    }

    /// Applies and saves the interface language; every window re-renders in it.
    fn set_language(&mut self, language: LanguagePref, cx: &mut Context<Self>) {
        self.prefs.language = language;
        self.save_prefs();
        i18n::set_language(language.resolve(self.system_lang));
        if let Some((_, settings)) = &self.settings {
            settings.update(cx, |s, cx| {
                s.language = language;
                cx.notify();
            });
        }
        cx.refresh_windows();
        cx.notify();
    }

    /// First frame after a language switch: menus, input placeholders, column
    /// headers and text stored by modules are rebuilt; drafts, selection and scroll
    /// positions are kept.
    fn relocalize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        set_app_menus(cx);
        if let Some(old) = self.config_error.clone() {
            // Re-word the configuration error; resolving only reads, as at startup.
            if let Err(message) = Startup::from_environment().connection {
                if self.notice.as_deref() == Some(config_notice_in_old(&old).as_str()) {
                    self.notice = Some(config_notice(&message));
                }
                self.config_error = Some(message);
            }
        }
        self.library.update(cx, |l, cx| l.relocalize(window, cx));
        self.reader.update(cx, |r, cx| r.relocalize(cx));
        self.sidebar.update(cx, |_, cx| cx.notify());
        self.list.update(cx, |_, cx| cx.notify());
        self.metadata.update(cx, |_, cx| cx.notify());
        self.chat.update(cx, |_, cx| cx.notify());
    }

    /// Reads the agent's capabilities again for the chat and the reader.
    fn refresh_capabilities(&mut self, cx: &mut Context<Self>) {
        self.client.forget_capabilities();
        self.chat.update(cx, |c, cx| c.probe(cx));
        self.reader.update(cx, |r, cx| r.probe_capability(cx));
        self.importer.update(cx, |i, cx| i.probe(cx));
    }

    /// Files dropped from Finder: on a sidebar row they go there; anywhere else
    /// into the current place (a library or folder), or Inbox from a virtual view.
    fn import_files(&mut self, paths: Vec<std::path::PathBuf>, spot: DropSpot, cx: &mut Context<Self>) {
        let library = self.library.read(cx);
        let destination = memex_library::import::destination(&library.model, library.show_trash, &spot);
        self.end_file_drag(cx);
        self.importer.update(cx, |i, cx| i.drop_paths(paths, destination, cx));
    }

    fn end_file_drag(&mut self, cx: &mut Context<Self>) {
        self.file_drag = None;
        self.library.update(cx, |l, _| l.drop_hover = None);
        cx.notify();
    }

    /// The soft layer over the window while files are dragged in: where a drop
    /// would go, and what is accepted.
    fn drop_hint(&self, count: usize, cx: &App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let library = self.library.read(cx);
        let spot = library.drop_hover.clone().unwrap_or(DropSpot::Current);
        let destination = memex_library::import::destination(&library.model, library.show_trash, &spot);
        let closed = self.importer.read(cx).supported == Some(false);
        let title = if closed {
            tx("Mac 端暂未开放文件导入").to_string()
        } else if destination.read_only() {
            txf("「{0}」是只读的，不能导入到这里", &[&destination.label()])
        } else {
            txf("松开以导入到 {0}", &[&destination.label()])
        };
        let icon = match destination { memex_library::import::Destination::Inbox => IconName::Inbox, _ => IconName::FolderOpen };
        let subtitle = txf("{0} 项 · 支持 {1} · 文件夹最多收集 {2} 个文件", &[&count, &memex_client::file_import::accepted_summary(), &memex_client::file_import::MAX_FILES_PER_DROP]);
        let limits = txf("PDF 不超过 {0} MiB，须带文本层（暂不支持 OCR）", &[&(memex_client::file_import::MAX_PDF_BYTES / 1024 / 1024)]);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme.background.opacity(0.32))
            .child(div().absolute().top(px(8.)).left(px(8.)).right(px(8.)).bottom(px(8.)).rounded(px(18.)).border_2().border_dashed().border_color(theme.primary.opacity(0.55)))
            .child(
                h_flex().absolute().left_0().bottom(px(72.)).w_full().justify_center().child(
                    v_flex()
                        .debug_selector(|| "file-drop-hint".into())
                        .items_center()
                        .gap_1p5()
                        .px_8()
                        .py_5()
                        .max_w(px(560.))
                        .rounded(px(18.))
                        .border_1()
                        .border_color(theme.primary.opacity(0.3))
                        .bg(theme.popover.opacity(0.97))
                        .shadow_lg()
                        .child(div().size(px(44.)).rounded_full().bg(theme.accent).flex().items_center().justify_center().child(Icon::new(if closed { IconName::Ban } else { icon }).text_color(theme.primary)))
                        .child(div().pt_1().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(theme.foreground).child(title))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(subtitle))
                        .when(!closed, |d| d.child(div().text_xs().text_color(theme.muted_foreground.opacity(0.85)).child(limits))),
                ),
            )
    }

    fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        theme::apply(self.prefs.appearance.is_dark(self.system_dark), cx);
        cx.notify();
    }

    /// Navigation guard: unsaved, saving or conflicting edits keep the current item.
    fn select(&mut self, item_id: String, new_tab: bool, cx: &mut Context<Self>) {
        let current = self.metadata.read(cx).item_id().map(str::to_string);
        if current.as_deref() == Some(item_id.as_str()) && !new_tab {
            let dates = self.library.read(cx).row_for(&item_id).map(|r| (r.published_at, r.duration_seconds));
            self.reader.update(cx, |r, cx| {
                if let Some((published_at, duration)) = dates { r.set_item_dates(&item_id, published_at, duration, cx); }
                r.probe_capability(cx);
            });
            return;
        }
        if self.metadata.read(cx).blocks_navigation(cx) || self.reader.read(cx).blocks_navigation(cx) {
            self.notice = Some(GUARD_NOTICE.into());
            self.library.update(cx, |l, cx| l.commit_selection(current, cx));
            cx.notify();
            return;
        }
        self.notice = None;
        let row = self.library.read(cx).row_for(&item_id);
        let facts = row
            .as_ref()
            .map(|r| ItemFacts { item_type: r.item_type.clone(), source: r.source_label.clone(), added_at: r.added_at.clone() })
            .unwrap_or_default();
        let dates = row.as_ref().map(|r| (r.published_at.clone(), r.duration_seconds)).unwrap_or_default();
        let title = row.map(|r| r.display_title()).unwrap_or_default();
        self.library.update(cx, |l, cx| l.commit_selection(Some(item_id.clone()), cx));
        self.metadata.update(cx, |m, cx| m.load(Some(item_id.clone()), facts, cx));
        self.reader.update(cx, |r, cx| {
            r.set_item_dates(&item_id, dates.0, dates.1, cx);
            r.open(&item_id, &title, new_tab, cx);
        });
        if new_tab {
            self.set_view(ViewMode::Reading, cx);
        }
        cx.notify();
    }

    fn set_view(&mut self, view: ViewMode, cx: &mut Context<Self>) {
        self.prefs.view = view;
        self.reader.update(cx, |r, cx| r.pass_view_shown(view == ViewMode::Reading, cx));
        self.metadata.update(cx, |m, cx| {
            m.layout = if view == ViewMode::Classic { Layout::Inspector } else { Layout::Compact };
            cx.notify();
        });
        self.save_prefs();
        cx.notify();
    }

    /// Toolbar quick toggle: folder lists show direct items or the whole subtree.
    fn set_include_subfolders(&mut self, on: bool, cx: &mut Context<Self>) {
        self.prefs.include_subfolders = on;
        self.save_prefs();
        self.library.update(cx, |l, cx| l.set_include_descendants(on, cx));
        cx.notify();
    }

    fn save_prefs(&self) {
        if let Some(path) = &self.prefs_path {
            self.prefs.save(path);
        }
    }

    fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.prefs.appearance = appearance;
        self.appearance_menu = false;
        self.save_prefs();
        self.apply_appearance(cx);
    }

    fn set_fixture(&mut self, f: impl FnOnce(&mut FixtureControls), cx: &mut Context<Self>) {
        if let Some(fixture) = &self.fixture {
            let mut controls = fixture.controls();
            f(&mut controls);
            fixture.set_controls(controls);
        }
        self.library.update(cx, |l, cx| l.reload(cx));
        cx.notify();
    }

    fn simulate_external_edit(&mut self, cx: &mut Context<Self>) {
        if let (Some(fixture), Some(id)) = (&self.fixture, self.metadata.read(cx).item_id()) {
            fixture.simulate_external_edit(id);
        }
        self.notice = Some("开发模式：已模拟在别处修改当前条目".into()); // a key, shown via relabel
        cx.notify();
    }

    fn toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state = self.library.read(cx).model.state.clone();
        let (dot, status) = match (&self.fixture, &state) {
            _ if self.config_error.is_some() => (theme.danger, tx("配置错误").to_string()),
            // Offline only when the socket could not be reached; a reachable agent that
            // failed a request is shown as an error, not as offline.
            (_, LoadState::Failed(m)) if relabel(m) == memex_client::ClientError::Unavailable.message() => (theme.muted_foreground, tx("代理离线").to_string()),
            (_, LoadState::Failed(_)) => (theme.warning, tx("代理异常").to_string()),
            (Some(_), _) => (theme.primary, tx("合成数据 · 开发模式").to_string()),
            (None, LoadState::Loading) => (theme.muted_foreground, tx("正在连接…").to_string()),
            (None, LoadState::Ready) => (gpui_kit::hsla(0.38, 0.45, 0.45, 1.), tx("代理已连接").to_string()),
        };
        let view = self.prefs.view;
        let include_subfolders = self.prefs.include_subfolders;
        // Only meaningful when the backend has a folder tree.
        let has_groups = !self.library.read(cx).model.groups().is_empty();
        let appearance_icon = if self.prefs.appearance.is_dark(self.system_dark) { IconName::Moon } else { IconName::Sun };
        TitleBar::new().child(
            h_flex()
                .flex_1()
                .gap_2()
                .pr_3()
                .child(Button::new("sidebar").ghost().small().icon(IconName::PanelLeft).tooltip(tx("显示/隐藏侧栏")).on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(if view == ViewMode::Classic { txf("{0} — 全部条目", &[&memex_client::BRAND_NAME]) } else { memex_client::BRAND_NAME.to_string() }))
                .child(div().text_xs().text_color(theme.muted_foreground).child(concat!("v", env!("CARGO_PKG_VERSION"))))
                .child(div().flex_1())
                .when(self.fixture.is_some(), |d| {
                    d.child(Button::new("dev-edit").ghost().xsmall().label(tx("模拟他处修改")).on_click(cx.listener(|this, _, _, cx| this.simulate_external_edit(cx))))
                        .child(Button::new("dev-offline").ghost().xsmall().label(tx("切换离线")).on_click(cx.listener(|this, _, _, cx| this.set_fixture(|c| c.offline = !c.offline, cx))))
                })
                .child(Button::new("refresh").ghost().small().icon(IconName::RefreshCw).tooltip(tx("刷新")).on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |l, cx| l.reload(cx)))))
                .child(h_flex().gap_1().px_2().py(px(2.)).rounded_full().border_1().border_color(theme.border).text_xs().child(div().size(px(7.)).rounded_full().bg(dot)).child(status))
                .when(has_groups, |d| {
                    d.child(
                        Switch::new("include-subfolders")
                            .small()
                            .checked(include_subfolders)
                            .label(tx("含子文件夹条目"))
                            .tooltip(tx("文件夹列表包含其子文件夹中的条目"))
                            .on_change(cx.listener(|this, on: &bool, _, cx| this.set_include_subfolders(*on, cx))),
                    )
                })
                .child(
                    h_flex()
                        .rounded(px(7.))
                        .bg(theme.muted)
                        .p(px(2.))
                        .child(Button::new("view-reading").small().label(tx("阅读")).icon(IconName::BookOpen).when(view == ViewMode::Reading, |b| b.primary()).when(view != ViewMode::Reading, |b| b.ghost()).tooltip(tx("阅读视图 ⌘1")).on_click(cx.listener(|this, _, _, cx| this.set_view(ViewMode::Reading, cx))))
                        .child(Button::new("view-classic").small().label(tx("经典")).icon(IconName::LayoutDashboard).when(view == ViewMode::Classic, |b| b.primary()).when(view != ViewMode::Classic, |b| b.ghost()).tooltip(tx("经典视图 ⌘2")).on_click(cx.listener(|this, _, _, cx| this.set_view(ViewMode::Classic, cx)))),
                )
                .child(Button::new("settings").ghost().small().icon(IconName::Settings).tooltip(tx("设置 ⌘,")).on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))))
                .child(
                    Button::new("appearance")
                        .ghost()
                        .small()
                        .icon(appearance_icon)
                        .dropdown_caret(true)
                        .tooltip(tx("外观"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.appearance_menu = !this.appearance_menu;
                            cx.notify();
                        })),
                ),
        )
    }

    /// ⌘J: folds the conversation pane to its header strip, or unfolds it.
    fn toggle_conversation(&mut self, cx: &mut Context<Self>) {
        let collapsed = !self.prefs.conversation_collapsed;
        self.set_conversation_collapsed(collapsed, cx);
    }

    fn set_conversation_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.prefs.conversation_collapsed != collapsed {
            self.prefs.conversation_collapsed = collapsed;
            self.save_prefs();
        }
        cx.notify();
    }

    /// Double click on the divider: the default reader share again.
    fn reset_center_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fraction = prefs::Prefs::default().center_split;
        self.prefs.center_split = fraction;
        self.center_applied = None;
        self.save_prefs();
        self.center_split.update(cx, |s, cx| {
            let container = s.container_size();
            if container > px(0.) {
                s.resize_panel(0, container * fraction, window, cx);
            }
        });
        cx.notify();
    }

    /// Divider between reader and conversation: a hairline with a centred grip
    /// that turns rose under the pointer or while dragged; double click resets.
    fn center_divider(&self, cx: &mut Context<Self>) -> gpui_kit::base::ResizeHandleRenderer {
        let shell = cx.entity().downgrade();
        std::rc::Rc::new(move |handle, _window, cx| {
            let theme = cx.theme().clone();
            let state = handle.state();
            let lit = state != gpui_kit::base::ResizeHandleState::Idle;
            let shell = shell.clone();
            Some(
                div()
                    .flex_none()
                    .relative()
                    .w_full()
                    .h(px(1.))
                    .bg(if lit { theme.primary.opacity(0.75) } else { memex_chat::surface::for_theme(theme.background).edge })
                    .child(
                        h_flex()
                            .absolute()
                            .top(px(-2.))
                            .left_0()
                            .w_full()
                            .justify_center()
                            .child(div().w(px(36.)).h(px(5.)).rounded_full().bg(if lit { theme.primary } else { theme.muted_foreground.opacity(0.35) })),
                    )
                    .child(
                        div()
                            .id("center-divider-reset")
                            .absolute()
                            .top(px(-4.))
                            .left_0()
                            .w_full()
                            .h(px(9.))
                            .on_click(move |event, window, cx| {
                                if event.click_count() == 2 {
                                    shell.update(cx, |this, cx| this.reset_center_split(window, cx)).ok();
                                }
                            }),
                    )
                    .into_any_element(),
            )
        })
    }

    fn allow_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.reader.read(cx).blocks_navigation(cx) || self.metadata.read(cx).blocks_navigation(cx) {
            self.notice = Some("还有未保存的修改或未确认的保存。请先保存，或明确放弃修改，再关闭窗口。".into());
            cx.notify();
            false
        } else { true }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.prefs.sidebar_visible = !self.prefs.sidebar_visible;
        self.save_prefs();
        cx.notify();
    }

    fn appearance_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let current = self.prefs.appearance;
        let mut menu = v_flex().w(px(160.)).p_1().rounded(px(8.)).border_1().border_color(theme.border).bg(theme.popover).shadow_md().text_sm();
        for (i, a) in [Appearance::System, Appearance::Light, Appearance::Dark].into_iter().enumerate() {
            menu = menu.child(
                h_flex()
                    .id(("appearance", i))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .hover(|d| d.bg(theme.muted))
                    .child(div().w(px(14.)).when(a == current, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(theme.primary))))
                    .child(a.label())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_appearance(a, cx))),
            );
        }
        deferred(anchored().position(point(px(0.), px(0.))).child(div().absolute().top(px(40.)).right(px(12.)).child(menu))).with_priority(1)
    }

    fn panel_header(&self, label: &'static str, button: AnyElement, cx: &App) -> Div {
        h_flex()
            .h(px(32.))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(button)
            .child(div().flex_1())
            .child(label)
    }

    fn reading(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let show_trash = self.library.read(cx).show_trash;
        let has_conversation = self.chat.read(cx).model.has_conversation();
        let composer = self.chat.update(cx, |c, cx| c.render_composer(window, cx));
        let collapsed = self.prefs.conversation_collapsed;
        if !has_conversation || collapsed || show_trash {
            self.reader.update(cx, |r, cx| r.set_compact(false, cx));
        }
        // Folded, nothing of the conversation is drawn (its entity keeps the draft
        // and history); the composer is not even rendered.
        let composer = (!collapsed).then_some(composer);
        // A folded composer is not drawn: hand its focus back to the shell so ⌘J
        // (and the other shortcuts) still reach it.
        if collapsed && self.chat.read(cx).composer().read(cx).focus_handle(cx).is_focused(window) {
            window.focus(&self.focus, cx);
        }
        let header = (has_conversation && !collapsed).then(|| self.chat.update(cx, |c, cx| c.render_header(collapsed, cx)));
        // Every pane clips to the height it is given: nothing paints into its neighbour.
        let clip = |child: AnyElement| div().size_full().min_h_0().overflow_hidden().child(child);
        // The conversation sits on its own rose-tinted surface (memex_chat::surface),
        // ruled off from the article, with a soft inset shade under its header.
        let theme = cx.theme().clone();
        let top = self.conversation_top.clone();
        let tone = memex_chat::surface::for_theme(theme.background);
        let pane = |header: AnyElement, transcript: Option<AnyElement>| {
            v_flex()
                .relative()
                .size_full()
                .min_h_0()
                .overflow_hidden()
                .bg(tone.surface)
                .border_t_1()
                .border_color(tone.edge)
                .child(canvas(move |b, _, _| top.set(b.origin.y), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                .child(div().flex_none().bg(tone.header).border_b_1().border_color(tone.edge.opacity(0.6)).child(header))
                .when_some(transcript, |d, t| {
                    d.child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(t)
                            .child(div().absolute().top_0().left_0().w_full().h(px(memex_chat::surface::INSET_HEIGHT)).bg(linear_gradient(180., linear_color_stop(tone.inset, 0.), linear_color_stop(tone.inset.opacity(0.), 1.)))),
                    )
                })
        };
        let center = v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(if show_trash {
                // 回收站 replaces the reading pane; trashed items cannot be opened.
                div().flex_1().min_h_0().overflow_hidden().child(self.trash.clone()).into_any_element()
            } else if collapsed {
                // The reader keeps its stats line at the bottom; the AI button sits
                // centered on that line.
                let theme = cx.theme().clone();
                let questions = self.chat.read(cx).model.question_count();
                let waiting = self.chat.read(cx).model.is_waiting();
                let open = h_flex()
                    .id("ai-unfold")
                    .gap_1p5()
                    .items_center()
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .cursor_pointer()
                    .border_1()
                    .border_color(theme.primary.opacity(0.45))
                    .bg(theme.background)
                    .shadow_sm()
                    .text_xs()
                    .hover(|d| d.bg(theme.primary.opacity(0.08)))
                    .child(div().px_1().rounded(px(4.)).bg(theme.primary).text_color(theme.primary_foreground).font_weight(FontWeight::BOLD).child("AI"))
                    .child(div().text_color(theme.foreground).child(if questions > 0 { txf("对话 · {0}", &[&questions]) } else { tx("对话").to_string() }))
                    .when(waiting, |d| d.child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)))
                    .child(Icon::new(IconName::ChevronUp).xsmall().text_color(theme.muted_foreground))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tx("展开 AI 对话 ⌘J")).build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.set_conversation_collapsed(false, cx)));
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.reader.clone())
                    .child(h_flex().absolute().left_0().bottom(px(9.)).w_full().justify_center().child(open))
                    .into_any_element()
            } else if let Some(header) = header {
                {
                    // The remembered share of the reader once the group has been measured:
                    // applied to each new split height (first layout, window resize), so
                    // the saved fraction holds and not the placeholder size of the first frame.
                    let container = self.center_split.read(cx).container_size();
                    if container > px(0.) && self.center_applied.is_none_or(|h| (h - container).abs() > px(0.5)) {
                        self.center_applied = Some(container);
                        let fraction = self.prefs.center_split;
                        self.center_split.update(cx, |s, cx| s.resize_panel(0, container * fraction, window, cx));
                        cx.notify();
                    }
                    // A short reader keeps a one-line title so some article stays in view.
                    let reader_h = self.center_split.read(cx).sizes().first().copied().unwrap_or(px(1000.));
                    self.reader.update(cx, |r, cx| r.set_compact(reader_h < px(300.), cx));
                    let reader_px = if container > px(0.) { container * self.prefs.center_split } else { px(520. * self.prefs.center_split / 0.58) };
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(
                            v_resizable("center-split")
                                .with_state(&self.center_split)
                                .with_handle_appearance(self.center_divider(cx))
                                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                                    let s = state.read(cx);
                                    let container = s.container_size();
                                    if let Some(first) = s.sizes().first().copied().filter(|_| container > px(0.)) {
                                        // The true share; the pixel minimums of the two panes are the limits.
                                        this.prefs.center_split = (first / container).clamp(0.05, 0.95);
                                        this.save_prefs();
                                    }
                                }))
                                .child(resizable_panel().size(reader_px).size_range(px(120.)..Pixels::MAX).child(clip(self.reader.clone().into_any_element())))
                                .child(resizable_panel().size_range(px(140.)..Pixels::MAX).child(pane(header, Some(self.chat.clone().into_any_element())))),
                        )
                        .into_any_element()
                }
            } else {
                div().flex_1().min_h_0().overflow_hidden().child(self.reader.clone()).into_any_element()
            })
            .when_some(composer.filter(|_| !show_trash), |d, composer| {
                // Unfolded: the composer carries a fold control even before any conversation.
                d.child(
                    div().relative().flex_none().child(composer).child(
                        div().absolute().top(px(6.)).right(px(10.)).child(
                            Button::new("ai-fold")
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronDown)
                                .label(tx("收起 AI"))
                                .tooltip(tx("收起 AI 对话 ⌘J"))
                                .on_click(cx.listener(|this, _, _, cx| this.set_conversation_collapsed(true, cx))),
                        ),
                    ),
                )
            });
        let expand = Button::new("expand-classic")
            .ghost()
            .xsmall()
            .icon(IconName::ArrowLeft)
            .label(tx("展开为经典视图"))
            .on_click(cx.listener(|this, _, _, cx| this.set_view(ViewMode::Classic, cx)))
            .into_any_element();
        let right = v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .child(self.panel_header("", expand, cx))
            .child(div().flex_1().min_h_0().child(
                v_resizable("right-split")
                    .with_state(&self.right_split)
                    .child(resizable_panel().size(px(360. * self.prefs.right_split / 0.5)).size_range(px(140.)..Pixels::MAX).child(self.with_capture_card(self.list.clone().into_any_element(), cx)))
                    .child(resizable_panel().size_range(px(200.)..Pixels::MAX).child(self.metadata.clone())),
            ));
        h_resizable("reading-main")
            .with_state(&self.main_split)
            .child(resizable_panel().visible(self.prefs.sidebar_visible).size(px(self.prefs.sidebar_width)).size_range(px(160.)..px(360.)).fixed().child(div().size_full().bg(cx.theme().sidebar).child(self.sidebar.clone())))
            .child(resizable_panel().size_range(px(360.)..Pixels::MAX).child(center))
            .child(resizable_panel().size(px(self.prefs.right_width)).size_range(px(260.)..px(560.)).fixed().child(right))
            .into_any_element()
    }

    /// The 采集 card (URL captures and file imports), docked under the item list;
    /// hidden when there is nothing to show.
    fn capture_card(&self, cx: &App) -> Option<AnyElement> {
        self.importer.read(cx).visible().then(|| self.importer.clone().into_any_element())
    }

    fn with_capture_card(&self, list: AnyElement, cx: &App) -> AnyElement {
        v_flex().size_full().child(div().flex_1().min_h_0().child(list)).children(self.capture_card(cx)).into_any_element()
    }

    fn classic(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let back = Button::new("back-reading")
            .ghost()
            .xsmall()
            .icon(IconName::ArrowRight)
            .label(tx("返回阅读视图"))
            .on_click(cx.listener(|this, _, _, cx| this.set_view(ViewMode::Reading, cx)))
            .into_any_element();
        let search = self.library.read(cx).search.clone();
        h_resizable("classic-main")
            .with_state(&self.main_split)
            .child(resizable_panel().visible(self.prefs.sidebar_visible).size(px(self.prefs.sidebar_width)).size_range(px(160.)..px(360.)).fixed().child(div().size_full().bg(cx.theme().sidebar).child(self.sidebar.clone())))
            .child(
                resizable_panel().size_range(px(360.)..Pixels::MAX).child(
                    v_flex()
                        .size_full()
                        .child(div().p_2().border_b_1().border_color(cx.theme().border).child(gpui_kit::component::input::Input::new(&search).small().prefix(Icon::new(IconName::Search).small()).cleanable(true)))
                        .child(div().flex_1().min_h_0().child(if self.library.read(cx).show_trash { self.trash.clone().into_any_element() } else { self.table.clone().into_any_element() }))
                        .children(self.capture_card(cx)),
                ),
            )
            .child(
                resizable_panel()
                    .size(px(self.prefs.inspector_width))
                    .size_range(px(280.)..px(560.))
                    .fixed()
                    .child(v_flex().size_full().child(self.panel_header("", back, cx)).child(self.metadata.clone())),
            )
            .into_any_element()
    }
}

fn chat_key(chat: &Chat) -> Option<(String, String)> {
    chat.model.current()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rendered_lang != i18n::language() {
            self.rendered_lang = i18n::language();
            self.relocalize(window, cx);
        }
        if std::mem::take(&mut self.pending_settings) {
            self.open_settings(window, cx);
        }
        let theme = cx.theme().clone();
        let body = match self.prefs.view {
            ViewMode::Reading => self.reading(window, cx),
            ViewMode::Classic => self.classic(cx),
        };
        let notice = self.notice.as_deref().map(relabel);
        // The drag left without an exit event reaching us (or ended elsewhere).
        if self.file_drag.is_some() && !cx.has_active_drag() {
            self.file_drag = None;
            self.library.update(cx, |l, _| l.drop_hover = None);
        }
        let drop_hint = self.file_drag.map(|count| self.drop_hint(count, cx));
        v_flex()
            .id("shell")
            .key_context("MemexShell")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ShowReading, _, cx| this.set_view(ViewMode::Reading, cx)))
            .on_action(cx.listener(|this, _: &ShowClassic, _, cx| this.set_view(ViewMode::Classic, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleConversation, _, cx| this.toggle_conversation(cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.library.update(cx, |l, cx| l.reload(cx))))
            .on_action(cx.listener(|this, _: &ToggleReadingFont, _, cx| this.reader.update(cx, |r, cx| r.set_reading_font(memex_client::reading_font().other(), cx))))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            // Files from Finder: GPUI turns the platform drag into an `ExternalPaths` drag.
            .on_drag_move::<ExternalPaths>(cx.listener(|this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                let count = e.drag(cx).paths().len();
                if this.file_drag != Some(count) {
                    this.file_drag = Some(count);
                    cx.notify();
                }
            }))
            .on_file_drop_exit(cx.listener(|this, _, _, cx| this.end_file_drag(cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.import_files(paths.paths().to_vec(), DropSpot::Current, cx)))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(self.toolbar(cx))
            .when_some(notice, |d, n| {
                d.child(
                    h_flex()
                        .gap_2()
                        .px_4()
                        .py_1()
                        .bg(theme.accent)
                        .text_sm()
                        .child(Icon::new(IconName::Info).small().text_color(theme.primary))
                        .child(div().flex_1().child(n))
                        .child(Button::new("dismiss").ghost().xsmall().icon(IconName::Close).on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        }))),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .when(self.appearance_menu, |d| d.child(self.appearance_menu(cx)))
            .children(drop_hint)
    }
}

fn config_notice(message: &str) -> String {
    txf("桌面配置错误：{0}", &[&message])
}

/// The same notice worded in the language that was active before the switch.
fn config_notice_in_old(message: &str) -> String {
    let other = if i18n::language() == Lang::En { Lang::Zh } else { Lang::En };
    i18n::tx_in(other, "桌面配置错误：{0}").replace("{0}", message)
}

/// Application menus in the current interface language.
fn set_app_menus(cx: &mut App) {
    cx.set_menus(vec![
        Menu { name: memex_client::BRAND_NAME.into(), items: vec![MenuItem::action(tx("设置…"), OpenSettings), MenuItem::separator(), MenuItem::action(txf("退出 {0}", &[&memex_client::BRAND_NAME]), Quit)], disabled: false },
        Menu {
            name: tx("显示").into(),
            items: vec![
                MenuItem::action(tx("阅读视图"), ShowReading),
                MenuItem::action(tx("经典视图"), ShowClassic),
                MenuItem::separator(),
                MenuItem::action(tx("显示/隐藏侧栏"), ToggleSidebar),
                MenuItem::action(tx("显示/隐藏对话"), ToggleConversation),
                MenuItem::action(tx("切换阅读字体（衬线/无衬线）"), ToggleReadingFont),
                MenuItem::action(tx("刷新"), Refresh),
            ],
            disabled: false,
        },
    ]);
}

/// The bundled icon set plus the icons it lacks (Lucide, ISC licence).
struct Assets;

const TRASH_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 11v6"/><path d="M14 11v6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M3 6h18"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>"#;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if path == memex_library::trash::TRASH_ICON {
            return Ok(Some(std::borrow::Cow::Borrowed(TRASH_SVG.as_bytes())));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

/// Backend used when the configuration is unusable: never the fixture, never a default daemon.
struct Unconfigured;

impl memex_client::Backend for Unconfigured {
    fn send(&self, _: &memex_client::Command) -> Result<memex_client::Response, memex_client::ClientError> {
        Err(memex_client::ClientError::Unavailable)
    }
    fn mode(&self) -> memex_client::BackendMode {
        memex_client::BackendMode::Agent { socket: std::path::PathBuf::new() }
    }
}

/// Development: MEMEX_DESKTOP_DISPLAY=<CGDirectDisplayID> marks an unattended
/// run: windows open on that display without taking focus, as an accessory app
/// that never activates; when the display is not connected they are not shown
/// at all (the walk renders its frames off screen).
fn dev_unattended() -> bool {
    std::env::var_os("MEMEX_DESKTOP_DISPLAY").is_some()
}

fn dev_display(cx: &App) -> Option<DisplayId> {
    std::env::var("MEMEX_DESKTOP_DISPLAY").ok().and_then(|v| v.parse::<u64>().ok()).map(DisplayId::new).filter(|id| cx.find_display(*id).is_some())
}

/// The app's shortcuts (also bound by the UI tests, so they exercise the same keymap).
fn key_bindings() -> Vec<KeyBinding> {
    let mut keys = memex_library::view::key_bindings();
    keys.extend(memex_reader::view::longread::key_bindings());
    keys.extend([
        KeyBinding::new("cmd-1", ShowReading, None),
        KeyBinding::new("cmd-2", ShowClassic, None),
        KeyBinding::new("cmd-alt-s", ToggleSidebar, None),
        KeyBinding::new("cmd-j", ToggleConversation, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    keys
}

#[cfg(test)]
mod ui_tests;

/// 寒蝉拙楷 (SIL OFL 1.1; the license ships in the app's Resources) is embedded
/// and registered once; the family it registered under is what the reader asks for.
fn register_bundled_fonts(cx: &mut App) {
    static KAI: &[u8] = include_bytes!("../../../assets/fonts/ChillZhuoKai_R.otf");
    let before: std::collections::HashSet<String> = cx.text_system().all_font_names().into_iter().collect();
    if let Err(e) = cx.text_system().add_fonts(vec![std::borrow::Cow::Borrowed(KAI)]) {
        eprintln!("memex: bundled font not registered: {e}");
        return;
    }
    let names = cx.text_system().all_font_names();
    let found = names.iter().find(|n| n.as_str() == memex_client::typography::KAI_FAMILY_IN_FILE)
        .or_else(|| names.iter().find(|n| !before.contains(*n) && (n.contains("寒蝉") || n.contains("ChillZhuoKai"))))
        .or_else(|| names.iter().find(|n| n.contains("寒蝉") || n.contains("ChillZhuoKai")));
    if let Some(name) = found { memex_client::set_kai_family(name.clone()); }
}

fn main() {
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        register_bundled_fonts(cx);
        cx.bind_keys(key_bindings());
        // Menus are (re)built by the shell once the interface language is known.
        // Development: MEMEX_DESKTOP_DISPLAY=<CGDirectDisplayID> opens the window on
        // that display (e.g. a spare external screen); unknown ids fall back to primary.
        let display = dev_display(cx);
        // An unattended walk runs as an accessory app so it never becomes the active
        // app or takes the menu bar; without its spare display it is not shown at all.
        let unattended = dev_unattended();
        if unattended {
            cx.set_activation_policy(ActivationPolicy::Accessory);
        }
        let mut options = TitleBar::window_options();
        options.display_id = display;
        options.focus = !unattended;
        options.show = !unattended || display.is_some();
        options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(display, size(px(1440.), px(900.)), cx)));
        options.window_min_size = Some(size(px(960.), px(620.)));
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Shell::new(window, cx))).expect("open window");
        // An unattended run must not take focus from the screen in use.
        if !unattended {
            cx.activate(true);
        }
    });
}
