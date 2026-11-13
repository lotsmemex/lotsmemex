//! Headless UI tests on GPUI's test platform: the real `Shell` on the synthetic
//! fixture, a temporary `layout.json`, and input sent through the window's own
//! event, keymap and input-handler paths (no OS window, no focus change).

use super::*;
use memex_client::fixture::{MINUTES_FAIL_ITEM, SHOWCASE_ITEM};
use memex_reader::insights::ButtonState;
use std::{path::PathBuf, time::Duration};

const CHINESE_ITEM: &str = "item_fixture_000008";

/// A fresh, empty directory for one test's `layout.json`.
fn layout_path(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("memex-ui-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("layout.json")
}

fn saved(path: &PathBuf) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_default()).unwrap_or(serde_json::Value::Null)
}

/// The shell in a 1440×900 test window, fixture connected, `item` open in the reader.
fn open_shell<'a>(cx: &'a mut TestAppContext, path: &PathBuf, item: &str) -> (Entity<Shell>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(key_bindings());
    });
    let prefs = Prefs { language: LanguagePref::Zh, appearance: Appearance::Light, ..Prefs::default() };
    let startup = Startup { prefs, prefs_path: Some(path.clone()), connection: Ok(prefs::Connection::Fixture) };
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::with_startup(startup, Lang::Zh, window, cx));
    cx.simulate_resize(size(px(1440.), px(900.)));
    shell.update(cx, |s, _| {
        let f = s.fixture.as_ref().unwrap();
        f.set_controls(FixtureControls { instant_chat: true, fast_minutes: true, ..f.controls() });
    });
    cx.run_until_parked();
    let item = item.to_string();
    shell.update(cx, |s, cx| s.select(item, false, cx));
    settle(cx);
    (shell, cx)
}

/// Runs pending work and lays out two frames (split sizes are measured in prepaint).
fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
}

/// One question through the chat, so the center splits into reader and conversation.
fn start_conversation(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    let chat = shell.read_with(cx, |s, _| s.chat.clone());
    chat.update(cx, |c, cx| assert!(c.send("这篇的核心论点？", cx)));
    settle(cx);
    assert!(chat.read_with(cx, |c, _| c.model.has_conversation() && !c.model.is_waiting()));
}

fn reader_share(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> (Pixels, Pixels, Pixels) {
    shell.read_with(cx, |s, cx| {
        let state = s.center_split.read(cx);
        (state.sizes()[0], state.sizes()[1], state.container_size())
    })
}

/// Press on the divider, move in steps with the button held, release.
fn drag_divider(shell: &Entity<Shell>, cx: &mut VisualTestContext, to_y: f32) {
    let x = px(560.);
    let top = shell.read_with(cx, |s, _| s.conversation_top.get());
    cx.simulate_mouse_down(point(x, top), MouseButton::Left, Modifiers::default());
    let steps = 6;
    for i in 1..=steps {
        let y = top + (px(to_y) - top) * (i as f32 / steps as f32);
        cx.simulate_mouse_move(point(x, y), MouseButton::Left, Modifiers::default());
    }
    cx.simulate_mouse_up(point(x, px(to_y)), MouseButton::Left, Modifiers::default());
    settle(cx);
}

#[::core::prelude::v1::test]
fn divider_drag_resizes_the_split_saves_on_mouse_up_and_honours_both_minimums() {
    let mut cx = TestAppContext::single();
    let path = layout_path("drag");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    start_conversation(&shell, cx);
    let (reader, chat, container) = reader_share(&shell, cx);
    assert!(container > px(400.), "split laid out: {container:?}");
    assert!((reader / container - 0.58).abs() < 0.02, "default share {reader:?}/{container:?}");
    let top = shell.read_with(cx, |s, _| s.conversation_top.get());
    let before = saved(&path)["center_split"].clone();

    // Up by 150 px: press, move (button held) — nothing saved before the release.
    let x = px(560.);
    cx.simulate_mouse_down(point(x, top), MouseButton::Left, Modifiers::default());
    for dy in [10., 40., 80., 120., 150.] {
        cx.simulate_mouse_move(point(x, top - px(dy)), MouseButton::Left, Modifiers::default());
    }
    settle(cx);
    let (moving, _, _) = reader_share(&shell, cx);
    assert!((moving - (reader - px(150.))).abs() < px(2.), "follows the pointer: {moving:?} vs {reader:?}");
    assert_eq!(saved(&path)["center_split"], before, "written only on mouse-up");
    cx.simulate_mouse_up(point(x, top - px(150.)), MouseButton::Left, Modifiers::default());
    settle(cx);
    let (after, chat_after, _) = reader_share(&shell, cx);
    assert!((after - (reader - px(150.))).abs() < px(2.));
    assert!((chat_after - (chat + px(150.))).abs() < px(2.));
    let written = saved(&path)["center_split"].as_f64().unwrap() as f32;
    assert!((written - after / container).abs() < 0.005, "saved {written} for {:?}", after / container);

    // Past the top: the reader stops at its 120 px minimum, and that share is what is saved.
    drag_divider(&shell, cx, 0.);
    let (min_reader, _, _) = reader_share(&shell, cx);
    assert!((min_reader - px(120.)).abs() < px(1.), "reader minimum: {min_reader:?}");
    let written = saved(&path)["center_split"].as_f64().unwrap() as f32;
    assert!((written - min_reader / container).abs() < 0.005, "saved {written}");

    // Past the bottom: the conversation keeps its 140 px minimum.
    drag_divider(&shell, cx, 2000.);
    let (max_reader, min_chat, _) = reader_share(&shell, cx);
    assert!((min_chat - px(140.)).abs() < px(1.), "conversation minimum: {min_chat:?}");
    let written = saved(&path)["center_split"].as_f64().unwrap() as f32;
    assert!((written - max_reader / container).abs() < 0.005, "saved {written}");

    // The saved share is what a restart reads back (after the load-time clamp).
    let reread = Prefs::load_lenient(&path);
    assert!((reread.center_split - written).abs() < 1e-6, "reloaded {}", reread.center_split);
}

#[::core::prelude::v1::test]
fn cmd_j_through_the_keymap_folds_and_unfolds_and_the_choice_persists() {
    let mut cx = TestAppContext::single();
    let path = layout_path("cmdj");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let collapsed = |shell: &Entity<Shell>, cx: &mut VisualTestContext| shell.read_with(cx, |s, _| s.prefs.conversation_collapsed);
    // The AI chat starts folded: more room to read.
    assert!(collapsed(&shell, cx));
    // Sending unfolds it.
    start_conversation(&shell, cx);
    assert!(!collapsed(&shell, cx));

    // Typing in the composer does not swallow ⌘J: it folds the whole chat away.
    let composer = shell.read_with(cx, |s, cx| s.chat.read(cx).composer().clone());
    composer.update_in(cx, |c, window, cx| c.focus(window, cx));
    cx.simulate_input("abc");
    cx.simulate_keystrokes("cmd-j");
    settle(cx);
    assert!(collapsed(&shell, cx));
    assert_eq!(saved(&path)["ai_folded"], serde_json::json!(true));
    // The keystroke goes through the window's keymap to the shell's action and unfolds.
    cx.simulate_keystrokes("cmd-j");
    settle(cx);
    assert!(!collapsed(&shell, cx));
    assert_eq!(saved(&path)["ai_folded"], serde_json::json!(false));
    // Folding and unfolding keep the draft; ⌘J types nothing.
    assert_eq!(composer.read_with(cx, |c, _| c.value().to_string()), "abc");
    // A restart reads the choice back.
    cx.simulate_keystrokes("cmd-j");
    assert!(Prefs::load_lenient(&path).conversation_collapsed);
}

/// The 纪要 button's centre, as drawn in the last frame.
fn minutes_button(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Point<Pixels> {
    let b = shell.read_with(cx, |s, cx| s.reader.read(cx).minutes_button_bounds()).expect("纪要 button drawn");
    b.center()
}

fn button_state(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<ButtonState> {
    shell.read_with(cx, |s, cx| s.reader.read(cx).translate.minutes_button())
}

/// Lets the fixture's (fast) minutes finish and the reader's backoff polls run.
fn wait_minutes(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(30));
        cx.executor().advance_clock(Duration::from_secs(5));
        settle(cx);
        if !matches!(button_state(shell, cx), Some(ButtonState::Generating)) {
            return;
        }
    }
}

fn click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_click(at, Modifiers::default());
    settle(cx);
}

#[::core::prelude::v1::test]
fn minutes_button_clicked_with_the_mouse_generates_then_hides_and_shows_the_card() {
    let mut cx = TestAppContext::single();
    let path = layout_path("minutes");
    let (shell, cx) = open_shell(&mut cx, &path, CHINESE_ITEM);
    let reader = shell.read_with(cx, |s, _| s.reader.clone());
    assert_eq!(button_state(&shell, cx), Some(ButtonState::NotGenerated));
    let card = |cx: &mut VisualTestContext| reader.read_with(cx, |r, _| r.translate.minutes().map(|m| m.state.clone()));
    assert_eq!(card(cx), Some(memex_client::MinutesState::NotRequested));

    let at = minutes_button(&shell, cx);
    click(cx, at);
    assert_eq!(button_state(&shell, cx), Some(ButtonState::Generating));
    assert!(reader.read_with(cx, |r, _| r.translate.minutes().is_some_and(|m| m.is_pending())), "the request went out and the card follows it");
    wait_minutes(&shell, cx);
    assert_eq!(button_state(&shell, cx), Some(ButtonState::Done));
    assert!(reader.read_with(cx, |r, _| r.translate.minutes().and_then(|m| m.text()).is_some()));
    assert!(!reader.read_with(cx, |r, _| r.minutes_card_hidden()));

    // 已完成: each click hides or shows the card; nothing is requested again.
    let at = minutes_button(&shell, cx);
    click(cx, at);
    assert!(reader.read_with(cx, |r, _| r.minutes_card_hidden()));
    assert_eq!(button_state(&shell, cx), Some(ButtonState::Done));
    let at = minutes_button(&shell, cx);
    click(cx, at);
    assert!(!reader.read_with(cx, |r, _| r.minutes_card_hidden()));
}

#[::core::prelude::v1::test]
fn minutes_failure_then_retry_with_the_mouse() {
    let mut cx = TestAppContext::single();
    let path = layout_path("minutes-fail");
    let (shell, cx) = open_shell(&mut cx, &path, MINUTES_FAIL_ITEM);
    assert_eq!(button_state(&shell, cx), Some(ButtonState::NotGenerated));
    let at = minutes_button(&shell, cx);
    click(cx, at);
    assert_eq!(button_state(&shell, cx), Some(ButtonState::Generating));
    wait_minutes(&shell, cx);
    let Some(ButtonState::Failed(reason)) = button_state(&shell, cx) else { panic!("{:?}", button_state(&shell, cx)) };
    assert!(reason.contains("超时"), "{reason}");
    // 失败: the same button retries.
    let at = minutes_button(&shell, cx);
    click(cx, at);
    assert_eq!(button_state(&shell, cx), Some(ButtonState::Generating));
    wait_minutes(&shell, cx);
    assert_eq!(button_state(&shell, cx), Some(ButtonState::Done));
}

#[::core::prelude::v1::test]
fn ime_composition_through_the_platform_input_handler_is_never_sent_by_enter() {
    let mut cx = TestAppContext::single();
    let path = layout_path("ime");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    // The composer is only drawn (and registers its input handler) unfolded.
    shell.update(cx, |s, cx| s.set_conversation_collapsed(false, cx));
    settle(cx);
    let chat = shell.read_with(cx, |s, _| s.chat.clone());
    let composer = chat.read_with(cx, |c, _| c.composer().clone());
    assert!(chat.read_with(cx, |c, _| c.model.can_send()));
    composer.update_in(cx, |c, window, cx| c.focus(window, cx));
    settle(cx);
    // Committed keys go through the input handler the textarea registered with the window.
    cx.simulate_input("Q");
    assert_eq!(composer.read_with(cx, |c, _| c.value().to_string()), "Q");

    // What macOS calls for an NSTextInputClient: the same PlatformInputHandler
    // type, wrapping the composer's own ElementInputHandler.
    let mut ime = cx.update(|window, cx| PlatformInputHandler::new(window.to_async(cx), Box::new(ElementInputHandler::new(Bounds::default(), composer.clone()))));
    // setMarkedText: pinyin while composing.
    ime.replace_and_mark_text_in_range(None, "ni'hao", Some(6..6));
    settle(cx);
    assert_eq!(composer.read_with(cx, |c, _| c.value().to_string()), "Qni'hao");
    assert_eq!(ime.marked_text_range(), Some(1..7), "hasMarkedText / markedRange");

    // Enter that the IME declined reaches GPUI as a bare key-down (macOS
    // `run_callback(KeyDown)`; no text is inserted on that path). It must not send.
    cx.simulate_event(KeyDownEvent { keystroke: Keystroke::parse("enter").unwrap(), is_held: false, prefer_character_input: false });
    settle(cx);
    assert!(!chat.read_with(cx, |c, _| c.model.has_conversation()), "nothing was sent while composing");
    assert_eq!(composer.read_with(cx, |c, _| c.value().to_string()), "Qni'hao");
    assert_eq!(ime.marked_text_range(), Some(1..7));

    // insertText: the candidate is committed, replacing the marked pinyin.
    ime.replace_text_in_range(None, "你好");
    settle(cx);
    assert_eq!(ime.marked_text_range(), None);
    assert_eq!(composer.read_with(cx, |c, _| c.value().to_string()), "Q你好");

    // Now Enter sends exactly the committed text, and the composer empties.
    cx.simulate_event(KeyDownEvent { keystroke: Keystroke::parse("enter").unwrap(), is_held: false, prefer_character_input: false });
    settle(cx);
    let transcript = chat.read_with(cx, |c, _| c.model.transcript().to_vec());
    assert_eq!(transcript.first().map(|m| m.text.as_str()), Some("Q你好"));
    assert!(transcript.iter().any(|m| m.speaker == memex_chat::model::Speaker::Assistant && m.text.contains("Q你好")), "the fixture answered the committed question");
    assert_eq!(composer.read_with(cx, |c, _| c.value().to_string()), "");
}

/// Backspace / Delete / ⌘⌫ on the selected item reach 移入本库垃圾桶 only while
/// the item list has focus. In a text field the same keys edit the text. (The
/// presentation fixture has no per-library trash, so it refuses the move; that
/// refusal shows the request was sent. The move itself is walked against the
/// synthetic Store: `MEMEX_DESKTOP_WALK=deletekey`.)
#[::core::prelude::v1::test]
fn delete_keys_trash_the_selected_item_only_from_the_list_never_from_a_text_field() {
    use memex_library::model::Filter;
    let mut cx = TestAppContext::single();
    let path = layout_path("deletekey");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let library = shell.read_with(cx, |s, _| s.library.clone());
    let notice = |cx: &mut VisualTestContext| library.read_with(cx, |l, _| l.trash_notice.clone());
    let focus_list = |cx: &mut VisualTestContext| {
        let focus = library.read_with(cx, |l, _| l.items_focus.clone());
        cx.update(|window, cx| window.focus(&focus, cx));
        settle(cx);
    };

    // 全部条目 has no library to trash into: the keys only say where deletion works.
    library.update(cx, |l, cx| l.set_filter(Filter::All, cx));
    settle(cx);
    library.update(cx, |l, _| l.group_trash_available = true);
    focus_list(cx);
    cx.simulate_keystrokes("backspace");
    settle(cx);
    assert_eq!(notice(cx).as_deref(), Some("进入一个库后可删除条目"));

    // Inside a library, with the list focused: each key sends the trash move.
    let root = library.read_with(cx, |l, _| l.model.groups().first().map(|g| g.root_group_id.clone())).expect("fixture library");
    library.update(cx, |l, cx| l.set_filter(Filter::Group(root.clone()), cx));
    settle(cx);
    library.update(cx, |l, _| l.group_trash_available = true);
    let first = library.read_with(cx, |l, _| l.model.row(0).map(|r| r.item_id.clone())).expect("a row in the library");
    shell.update(cx, |s, cx| s.select(first.clone(), false, cx));
    settle(cx);
    for key in ["backspace", "delete", "cmd-backspace"] {
        // The fixture's capabilities clear this again on every reload.
        library.update(cx, |l, _| { l.trash_notice = None; l.group_trash_available = true; });
        focus_list(cx);
        cx.simulate_keystrokes(key);
        settle(cx);
        let n = notice(cx).unwrap_or_default();
        assert!(n.starts_with("删除未确认"), "{key}: the move was sent ({n})");
    }

    // A text field keeps its own Backspace: the search box and the composer edit
    // text, even though the list would delete right now (checked before each key).
    let armed = |cx: &mut VisualTestContext| {
        library.update(cx, |l, _| { l.trash_notice = None; l.group_trash_available = true; });
        assert!(library.read_with(cx, |l, _| l.model.selected_index().is_some() && l.current_root().is_some() && !l.trash_busy), "the list would delete");
    };
    let title = library.read_with(cx, |l, _| l.row_for(&first).map(|r| r.title.chars().take(3).collect::<String>())).unwrap();
    let search = library.read_with(cx, |l, _| l.search.clone());
    search.update_in(cx, |s, window, cx| { s.set_value(title.clone(), window, cx); s.focus(window, cx) });
    settle(cx);
    armed(cx);
    cx.simulate_keystrokes("backspace");
    assert_eq!(notice(cx), None, "nothing was trashed from the search box");
    settle(cx);
    assert_eq!(search.read_with(cx, |s, _| s.value().to_string()), title.chars().take(2).collect::<String>());
    assert_eq!(notice(cx), None);
    search.update_in(cx, |s, window, cx| s.set_value("", window, cx));
    shell.update(cx, |s, cx| s.set_conversation_collapsed(false, cx));
    settle(cx);
    let composer = shell.read_with(cx, |s, cx| s.chat.read(cx).composer().clone());
    composer.update_in(cx, |c, window, cx| c.focus(window, cx));
    cx.simulate_input("问题");
    settle(cx);
    armed(cx);
    for key in ["backspace", "delete", "cmd-backspace"] { cx.simulate_keystrokes(key); }
    assert_eq!(notice(cx), None, "nothing was trashed from the composer");
    settle(cx);
    assert!(composer.read_with(cx, |c, _| c.value().to_string()).len() < "问题".len(), "the composer edited its own text");
    assert_eq!(notice(cx), None);
}

#[::core::prelude::v1::test]
fn guji_style_is_kept_per_item_display_only_and_defaults_vertical_to_kai() {
    use memex_client::fixture::GUJI_ITEM;
    use memex_client::opencc::Script;
    let mut cx = TestAppContext::single();
    let path = layout_path("guji");
    let (shell, cx) = open_shell(&mut cx, &path, GUJI_ITEM);
    let reader = shell.read_with(cx, |s, _| s.reader.clone());
    let stored = |cx: &mut VisualTestContext| reader.read_with(cx, |r, _| r.model.detail().map(|d| d.canonical.markdown.clone()).unwrap_or_default());
    let before = stored(cx);
    assert!(before.contains("无名天地之始"), "fixture text is simplified");
    assert_eq!(reader.read_with(cx, |r, _| r.item_font()), memex_client::ReadingFont::Sans);

    reader.update(cx, |r, cx| { r.set_vertical(true, cx); r.set_script(Script::Traditional, cx); });
    settle(cx);
    // 竖排 draws in 寒蝉拙楷 unless chosen otherwise; the stored text is untouched.
    assert_eq!(reader.read_with(cx, |r, _| r.item_font()), memex_client::ReadingFont::Kai);
    assert_eq!(stored(cx), before);
    assert_eq!(saved(&path)["item_styles"][GUJI_ITEM], serde_json::json!({"script": "traditional", "vertical": true}));

    // Another item keeps the app's choices; coming back restores this item's.
    shell.update(cx, |s, cx| s.select(CHINESE_ITEM.into(), false, cx));
    settle(cx);
    assert!(reader.read_with(cx, |r, _| r.item_style().is_default()));
    shell.update(cx, |s, cx| s.select(GUJI_ITEM.into(), false, cx));
    settle(cx);
    assert_eq!(reader.read_with(cx, |r, _| r.item_style()), memex_client::ItemStyle { script: Script::Traditional, vertical: true, font: None });

    // Back to 原文 · 横排: nothing is kept for the item.
    reader.update(cx, |r, cx| { r.set_vertical(false, cx); r.set_script(Script::Original, cx); });
    settle(cx);
    assert!(saved(&path)["item_styles"].get(GUJI_ITEM).is_none());
}

#[::core::prelude::v1::test]
fn inbox_is_the_default_never_mixed_with_a_folder_and_says_when_it_cannot_tell() {
    use memex_library::model::Filter;
    let mut cx = TestAppContext::single();
    let path = layout_path("inbox");
    let (shell, cx) = open_shell(&mut cx, &path, CHINESE_ITEM);
    let library = shell.read_with(cx, |s, _| s.library.clone());
    let state = |cx: &mut VisualTestContext| library.read_with(cx, |l, _| (l.model.filter(), l.model.spec.unfiled, l.model.spec.group.is_some(), l.model.inbox_count, l.model.inbox_notice().is_some(), l.model.len()));
    // Selected at start; the agent counts the unfiled items, newest added first.
    let (filter, unfiled, group, count, notice, len) = state(cx);
    assert_eq!((filter, unfiled, group, notice), (Filter::Inbox, true, false, false));
    assert_eq!(count, Some(len));
    assert_eq!(library.read_with(cx, |l, _| l.model.sort()), (memex_client::SortKey::AddedAt, true));
    let all = library.read_with(cx, |l, _| l.model.facets().total);
    assert!(len > 0 && len < all, "{len} of {all}");

    // A folder never carries `unfiled`; back in the Inbox no folder is sent.
    library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_sleep".into()), cx));
    settle(cx);
    assert_eq!(state(cx).1, false);
    library.update(cx, |l, cx| { l.set_include_descendants(true, cx); l.set_filter(Filter::Inbox, cx); });
    settle(cx);
    let (filter, unfiled, group, ..) = state(cx);
    assert_eq!((filter, unfiled, group), (Filter::Inbox, true, false));

    // A real double click on an editable folder opens its name field.
    let tree = shell.read_with(cx, |s, cx| s.sidebar.read(cx).tree());
    let bounds = cx.debug_bounds("group-grp_c_sleep").expect("folder row on screen");
    for click_count in [1, 2] {
        cx.simulate_event(MouseDownEvent { position: bounds.center(), button: MouseButton::Left, modifiers: Modifiers::default(), click_count, first_mouse: false });
        cx.simulate_event(MouseUpEvent { position: bounds.center(), button: MouseButton::Left, modifiers: Modifiers::default(), click_count });
    }
    settle(cx);
    assert_eq!(tree.read_with(cx, |t, _| t.renaming().map(str::to_string)), Some("grp_c_sleep".into()));

    // An agent without `query_unfiled`: everything, said so, no count, never `unfiled`.
    shell.update(cx, |s, _| { let f = s.fixture.as_ref().unwrap(); f.set_controls(FixtureControls { no_unfiled_query: true, ..f.controls() }); });
    library.update(cx, |l, cx| { l.client().forget_capabilities(); l.set_filter(Filter::Inbox, cx); });
    settle(cx);
    let (filter, unfiled, _, count, notice, len) = state(cx);
    assert_eq!((filter, unfiled, count, notice), (Filter::Inbox, false, None, true));
    assert_eq!(len, all);
}

#[::core::prelude::v1::test]
fn folder_delete_confirms_the_preview_rereads_everything_and_restores_in_order() {
    use memex_library::{model::Filter, view::DeleteFlow};
    let mut cx = TestAppContext::single();
    let path = layout_path("groupdelete");
    let (shell, cx) = open_shell(&mut cx, &path, CHINESE_ITEM);
    let library = shell.read_with(cx, |s, _| s.library.clone());
    let has = |cx: &mut VisualTestContext, id: &str| library.read_with(cx, |l, _| l.model.group(id).is_some());
    let inbox_before = library.read_with(cx, |l, _| l.model.inbox_count).unwrap();
    assert!(library.read_with(cx, |l, _| l.can_delete("grp_c_bias")));

    // Shown folder inside the subtree: preview → confirm → reread, back to Inbox.
    library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_anchor".into()), cx));
    settle(cx);
    library.update(cx, |l, cx| l.begin_delete("grp_c_bias".into(), None, cx));
    settle(cx);
    let preview = library.read_with(cx, |l, _| match &l.delete { Some(DeleteFlow::Confirm { preview, .. }) => preview.clone(), other => panic!("{other:?}") });
    assert_eq!(preview.affected_node_count, 3);
    // An expired token: a fresh preview with the reason, nothing resent or deleted.
    shell.update(cx, |s, _| { let f = s.fixture.as_ref().unwrap(); f.set_controls(FixtureControls { expire_delete_tokens: true, ..f.controls() }); });
    library.update(cx, |l, cx| l.confirm_delete(cx));
    settle(cx);
    assert!(has(cx, "grp_c_bias"));
    let notice = library.read_with(cx, |l, _| match &l.delete { Some(DeleteFlow::Confirm { notice, preview }) => (notice.clone(), preview.confirmation_token.clone()), other => panic!("{other:?}") });
    assert_eq!(notice.0.as_deref(), Some("内容已变化，请重新确认"));
    assert_ne!(notice.1, preview.confirmation_token);
    shell.update(cx, |s, _| { let f = s.fixture.as_ref().unwrap(); f.set_controls(FixtureControls { expire_delete_tokens: false, ..f.controls() }); });
    library.update(cx, |l, cx| l.confirm_delete(cx));
    settle(cx);
    let (filter, flow, deletions, inbox) = library.read_with(cx, |l, _| (l.model.filter(), l.delete.clone(), l.deletions.len(), l.model.inbox_count));
    assert_eq!((filter, flow, deletions), (Filter::Inbox, None, 1));
    assert!(!has(cx, "grp_c_bias") && !has(cx, "grp_c_anchor"));
    assert!(inbox.unwrap() >= inbox_before, "items left with no folder join the Inbox");
    // The request id was written beside the settings before sending, and cleared after.
    let store = path.parent().unwrap().join("group-delete-requests.json");
    assert_eq!(std::fs::read_to_string(&store).unwrap(), "{}");

    // Parent deleted too: restoring the child is refused with the fixed reason.
    library.update(cx, |l, cx| l.begin_delete("grp_c_cog".into(), None, cx));
    settle(cx);
    library.update(cx, |l, cx| l.confirm_delete(cx));
    settle(cx);
    let ids = library.read_with(cx, |l, _| l.deletions.iter().map(|d| (d.group_id.clone(), d.deletion_id.clone())).collect::<Vec<_>>());
    let of = |g: &str| ids.iter().find(|(id, _)| id == g).unwrap().1.clone();
    library.update(cx, |l, cx| l.restore_deletion(of("grp_c_bias"), cx));
    settle(cx);
    assert_eq!(library.read_with(cx, |l, _| l.deletion_notice.clone()).as_deref(), Some("请先恢复上级"));
    library.update(cx, |l, cx| l.restore_deletion(of("grp_c_cog"), cx));
    settle(cx);
    assert!(has(cx, "grp_c_cog") && !has(cx, "grp_c_bias"));
    assert_eq!(library.read_with(cx, |l, _| l.deletions.len()), 1);

    // Without `node_delete`: no entry, nothing sent.
    shell.update(cx, |s, _| { let f = s.fixture.as_ref().unwrap(); f.set_controls(FixtureControls { no_node_delete: true, ..f.controls() }); });
    library.update(cx, |l, cx| { l.client().forget_capabilities(); l.reload(cx); });
    settle(cx);
    assert!(library.read_with(cx, |l, _| !l.node_delete && !l.can_delete("grp_c_cog") && l.deletions.is_empty()));
    library.update(cx, |l, cx| l.begin_delete("grp_c_cog".into(), None, cx));
    assert!(library.read_with(cx, |l, _| l.delete.is_none()));
}

#[::core::prelude::v1::test]
fn long_read_restores_its_position_keeps_bookmarks_across_revisions_and_marks_by_shortcut() {
    use memex_client::fixture::LONGREAD_ITEM;
    use memex_reader::nav::Filter;
    let mut cx = TestAppContext::single();
    let path = layout_path("longread");
    let (shell, cx) = open_shell(&mut cx, &path, LONGREAD_ITEM);
    let reader = shell.read_with(cx, |s, _| s.reader.clone());
    let client = shell.read_with(cx, |s, _| s.client.clone());
    assert!(reader.read_with(cx, |r, _| r.lr.source.is_some() && r.lr.state.is_some() && !r.lr.terms.is_empty()), "{}", reader.read_with(cx, |r, _| r.dev_longread()));
    // Reading moves the top row: after a pause it is saved as a real source anchor.
    reader.update(cx, |r, cx| r.dev_scroll_to(12, cx));
    settle(cx);
    cx.executor().advance_clock(Duration::from_secs(2));
    settle(cx);
    let saved_state = client.reader_state(LONGREAD_ITEM).unwrap();
    let pos = saved_state.state.position.clone().expect("position saved");
    assert!(pos.side == "source" && pos.chapter.starts_with("section-") && pos.offset < 4096, "{pos:?}");
    // A bookmark at the middle paragraph; saving a position later keeps it.
    reader.update(cx, |r, cx| r.dev_add_bookmark(cx));
    settle(cx);
    assert_eq!(client.reader_state(LONGREAD_ITEM).unwrap().state.bookmarks.len(), 1);
    // Reopening returns to the saved paragraph.
    shell.update(cx, |s, cx| s.select(CHINESE_ITEM.into(), false, cx));
    settle(cx);
    shell.update(cx, |s, cx| s.select(LONGREAD_ITEM.into(), false, cx));
    settle(cx);
    let top = reader.read_with(cx, |r, _| r.dev_longread());
    assert!(top.contains("top=12") || top.contains("top=11") || top.contains("top=13"), "{top}");
    assert_eq!(client.reader_state(LONGREAD_ITEM).unwrap().state.bookmarks.len(), 1);

    // Marks: no selection → ⌘B is not taken; with a source selection it marks,
    // and a switch that hides manual marks goes back to 全部.
    reader.update(cx, |r, cx| r.set_filter(Filter::Ai, cx));
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(reader.read_with(cx, |r, _| r.dev_marks().0), 0);
    let md = reader.read_with(cx, |r, _| r.model.detail().unwrap().canonical.markdown.clone());
    let at = md.find("账单反而更高").unwrap();
    reader.update_in(cx, |r, window, cx| r.dev_select(at..at + "账单反而更高".len(), false, window, cx));
    cx.simulate_keystrokes("cmd-b");
    settle(cx);
    let (count, _, filter) = reader.read_with(cx, |r, _| r.dev_marks());
    assert_eq!((count, filter), (1, Filter::All));
    assert_eq!(client.annotations_list(LONGREAD_ITEM).unwrap().annotations.len(), 1);
    // A translation-side selection cannot be saved; nothing is sent.
    reader.update_in(cx, |r, window, cx| r.dev_select(at..at + 3, true, window, cx));
    cx.simulate_keystrokes("cmd-u");
    settle(cx);
    assert_eq!(client.annotations_list(LONGREAD_ITEM).unwrap().annotations.len(), 1);

    // A new revision: the old position no longer binds → 从开头开始, bookmarks kept;
    // the old mark is stale and not counted.
    shell.update(cx, |s, _| s.fixture.as_ref().unwrap().simulate_external_edit(LONGREAD_ITEM));
    reader.update(cx, |r, cx| r.reload(cx));
    settle(cx);
    let (notice, count) = reader.read_with(cx, |r, _| (r.lr.notice.clone(), r.dev_marks().0));
    assert_eq!(notice.as_deref(), Some("文章已更新，从开头开始"));
    assert_eq!(count, 0);
    assert_eq!(client.reader_state(LONGREAD_ITEM).unwrap().state.bookmarks.len(), 1);

    // Without `reader_annotations_v1`: kept in this session only, and said so.
    shell.update(cx, |s, _| { let f = s.fixture.as_ref().unwrap(); f.set_controls(FixtureControls { no_reader_annotations: true, ..f.controls() }); });
    client.forget_capabilities();
    reader.update(cx, |r, cx| r.reload(cx));
    settle(cx);
    let md = reader.read_with(cx, |r, _| r.model.detail().unwrap().canonical.markdown.clone());
    let at = md.find("成本").unwrap();
    reader.update_in(cx, |r, window, cx| r.dev_select(at..at + "成本".len(), false, window, cx));
    cx.simulate_keystrokes("cmd-y");
    settle(cx);
    let (count, notice, _) = reader.read_with(cx, |r, _| r.dev_marks());
    assert_eq!(count, 1);
    assert_eq!(notice.as_deref(), Some("本机暂存，未同步保存"));
}

#[::core::prelude::v1::test]
fn reading_timing_is_on_by_itself_and_creates_or_continues_a_pass_only_at_a_real_activity() {
    use memex_client::fixture::LONGREAD_ITEM;
    let mut cx = TestAppContext::single();
    let path = layout_path("auto-timing");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let reader = shell.read_with(cx, |s, _| s.reader.clone());
    let state = |cx: &mut VisualTestContext| reader.read_with(cx, |r, _| {
        let p = &r.passes;
        (p.clock.enabled, p.target.clone(), p.list.as_ref().map_or(0, |l| l.passes.len()), p.clock.session_id().is_some())
    });
    // Opening the article switches timing on and writes nothing.
    assert_eq!(state(cx), (true, None, 0, false));
    // The first real activity creates the tracked pass and starts its session.
    reader.update(cx, |r, cx| r.dev_pass_activity(cx));
    settle(cx);
    let (on, target, passes, session) = state(cx);
    assert!(on && target.is_some() && session, "created at the activity");
    assert_eq!(passes, 1);
    // Another activity reuses it.
    reader.update(cx, |r, cx| r.dev_pass_activity(cx));
    settle(cx);
    assert_eq!(state(cx).1, target);
    // An item with an unfinished tracked pass: that one is continued, nothing new is created.
    shell.update(cx, |s, cx| { s.fixture.as_ref().unwrap().seed_reading_passes(LONGREAD_ITEM); s.select(LONGREAD_ITEM.into(), false, cx); });
    settle(cx);
    let (on, open, before, _) = state(cx);
    assert!(on && open.is_some(), "the unfinished pass is the target");
    reader.update(cx, |r, cx| r.dev_pass_activity(cx));
    settle(cx);
    assert_eq!((state(cx).1, state(cx).2), (open, before));
    // ⏱ off by hand is remembered and stops the clock.
    reader.update(cx, |r, cx| r.pass_toggle_timing(cx));
    settle(cx);
    assert!(!state(cx).0);
    assert_eq!(saved(&path)["pass_timing_auto"], serde_json::json!(false));
}

/// What the same reading of the long read leaves behind: the cover on screen,
/// block coordinates, rows, a selection's source bytes, the saved mark and
/// the saved reading position.
fn cover_run(cover: bool) -> (Option<(String, f32)>, Vec<(usize, usize)>, usize, std::ops::Range<usize>, String, String, usize) {
    use memex_client::fixture::LONGREAD_ITEM;
    let mut cx = TestAppContext::single();
    let path = layout_path(if cover { "cover-on" } else { "cover-off" });
    let (shell, cx) = open_shell(&mut cx, &path, CHINESE_ITEM);
    let reader = shell.read_with(cx, |s, _| s.reader.clone());
    let client = shell.read_with(cx, |s, _| s.client.clone());
    shell.update(cx, |s, _| { let f = s.fixture.as_ref().unwrap(); f.set_controls(FixtureControls { no_item_cover: !cover, ..f.controls() }); });
    client.forget_capabilities();
    shell.update(cx, |s, cx| s.select(LONGREAD_ITEM.into(), false, cx));
    settle(cx);
    settle(cx);
    let (shown, blocks, rows, lead) = reader.read_with(cx, |r, _| (r.dev_cover(), r.model.original().iter().map(|b| (b.start, b.end)).collect::<Vec<_>>(), r.model.layout().rows.len(), r.dev_list_len()));
    let selection = reader.read_with(cx, |r, _| r.dev_selection_source("账单反而更高")).expect("term in the text");
    reader.update_in(cx, |r, window, cx| r.dev_select(selection.clone(), false, window, cx));
    cx.simulate_keystrokes("cmd-b");
    settle(cx);
    let marks = client.annotations_list(LONGREAD_ITEM).unwrap().annotations;
    assert_eq!(marks.len(), 1);
    reader.update(cx, |r, cx| r.dev_scroll_to(12, cx));
    settle(cx);
    cx.executor().advance_clock(Duration::from_secs(2));
    settle(cx);
    let pos = client.reader_state(LONGREAD_ITEM).unwrap().state.position.expect("position saved");
    (shown, blocks, rows, selection, format!("{:?}", marks[0].anchor), format!("{} {} {}", pos.side, pos.chapter, pos.offset), lead - rows)
}

#[::core::prelude::v1::test]
fn a_cover_sits_above_the_text_and_leaves_selection_mark_and_position_coordinates_unchanged() {
    let with = cover_run(true);
    let without = cover_run(false);
    let (version, ratio) = with.0.clone().expect("the synthetic cover is shown");
    assert_eq!(version.len(), 64);
    assert!((ratio - 16. / 9.).abs() < 0.01, "{ratio}");
    assert!(without.0.is_none(), "no capability: no cover");
    // The cover is not a row of the text: one lead row either way.
    assert_eq!((with.6, without.6), (1, 1));
    assert_eq!(with.1, without.1, "block byte ranges");
    assert_eq!(with.2, without.2, "rows");
    assert_eq!(with.3, without.3, "selection source bytes");
    assert_eq!(with.4, without.4, "saved mark anchor");
    assert_eq!(with.5, without.5, "saved reading position");
}

#[::core::prelude::v1::test]
fn only_explicit_sidebar_clicks_report_the_active_group() {
    use memex_library::model::Filter;
    let mut cx = TestAppContext::single();
    let path = layout_path("activegroup");
    let (shell, cx) = open_shell(&mut cx, &path, CHINESE_ITEM);
    let library = shell.read_with(cx, |s, _| s.library.clone());
    let fixture = shell.read_with(cx, |s, _| s.fixture.clone().unwrap());
    let calls = || fixture.active_group_calls();
    let row = |cx: &mut VisualTestContext, selector: &'static str| cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} on screen")).center();

    // Start-up (the restored Inbox), reloads, focus refreshes and capability refreshes say nothing.
    assert!(calls().is_empty(), "{:?}", calls());
    fixture.simulate_external_edit(CHINESE_ITEM);
    library.update(cx, |l, cx| { l.reload(cx); l.refresh_if_changed(cx); });
    shell.update(cx, |s, cx| s.refresh_capabilities(cx));
    settle(cx);
    assert!(calls().is_empty(), "{:?}", calls());

    // A click on a folder (not a root) sends its id; Inbox and 全部条目 send null.
    let folder = row(cx, "group-grp_c_sleep");
    click(cx, folder);
    assert_eq!(library.read_with(cx, |l, _| l.model.filter()), Filter::Group("grp_c_sleep".into()));
    let at = row(cx, "sidebar-inbox");
    click(cx, at);
    let at = row(cx, "sidebar-all");
    click(cx, at);
    let sent = calls();
    let groups: Vec<_> = sent.iter().map(|(_, g)| g.clone()).collect();
    assert_eq!(groups, vec![Some("grp_c_sleep".to_string()), None, None]);
    let ids: std::collections::HashSet<_> = sent.iter().map(|(r, _)| r.clone()).collect();
    assert_eq!(ids.len(), 3, "a new request id per click");
    assert_eq!(fixture.active_group(), None);

    // A library root reports its own id.
    let root = library.read_with(cx, |l, _| l.model.group("grp_c_sleep").map(|g| g.root_group_id.clone())).unwrap();
    library.update(cx, |l, cx| l.navigate(Filter::Group(root.clone()), cx));
    settle(cx);
    assert_eq!(calls().last().unwrap().1.as_deref(), Some(root.as_str()));

    // Rapid clicks: only the last one is sent; older ones are not queued again.
    let before = calls().len();
    library.update(cx, |l, cx| { l.navigate(Filter::Inbox, cx); l.navigate(Filter::Group(root.clone()), cx); l.navigate(Filter::Group("grp_c_sleep".into()), cx); });
    settle(cx);
    assert_eq!(calls()[before..].iter().map(|(_, g)| g.clone()).collect::<Vec<_>>(), vec![Some("grp_c_sleep".to_string())]);
    assert_eq!(fixture.active_group(), Some("grp_c_sleep".into()));

    // An unknown outcome: one retry with the same request id; the view is not held up.
    fixture.set_controls(FixtureControls { lose_active_group_replies: 1, ..fixture.controls() });
    let before = calls().len();
    let at = row(cx, "sidebar-inbox");
    click(cx, at);
    let retried = calls()[before..].to_vec();
    assert_eq!(retried.len(), 2);
    assert_eq!(retried[0], retried[1]);
    assert_eq!(retried[0].1, None);
    assert_eq!(library.read_with(cx, |l, _| l.model.filter()), Filter::Inbox);

    // Without the feature, clicks navigate and send nothing.
    fixture.set_controls(FixtureControls { no_active_group: true, ..fixture.controls() });
    library.update(cx, |l, _| l.client().forget_capabilities());
    let before = calls().len();
    click(cx, folder);
    let at = row(cx, "sidebar-all");
    click(cx, at);
    assert_eq!(calls().len(), before);
    assert_eq!(library.read_with(cx, |l, _| l.model.filter()), Filter::All);

    // A reopened window starts quiet as well.
    drop(shell);
    let mut cx2 = TestAppContext::single();
    let (shell, cx2) = open_shell(&mut cx2, &path, CHINESE_ITEM);
    assert!(shell.read_with(cx2, |s, _| s.fixture.clone().unwrap()).active_group_calls().is_empty());
}

/// A fresh directory of small synthetic files for one import test.
fn import_files(test: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("memex-ui-import-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("notes/deep")).unwrap();
    std::fs::write(dir.join("synthetic.pdf"), b"%PDF-1.7\nsynthetic").unwrap();
    std::fs::write(dir.join("notes/a.md"), b"# synthetic").unwrap();
    std::fs::write(dir.join("notes/deep/b.txt"), b"synthetic").unwrap();
    (dir.clone(), dir.join("synthetic.pdf"), dir.join("notes"))
}

fn external(paths: &[&PathBuf]) -> ExternalPaths {
    ExternalPaths(paths.iter().map(|p| p.to_path_buf()).collect())
}

/// Lets the import driver submit and poll (its poll timer included).
fn pump_imports(cx: &mut VisualTestContext) {
    for _ in 0..12 {
        cx.executor().advance_clock(memex_library::import_view::POLL_INTERVAL);
        cx.run_until_parked();
    }
    settle(cx);
}

fn tray_rows(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Vec<memex_library::import::ImportRow> {
    shell.read_with(cx, |s, cx| s.importer.read(cx).tray.rows().to_vec())
}

#[::core::prelude::v1::test]
fn finder_files_dropped_on_a_folder_row_import_there_and_open_from_the_tray() {
    use memex_library::import::{DropSpot, RowState};
    let mut cx = TestAppContext::single();
    let path = layout_path("import-folder");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let fixture = shell.read_with(cx, |s, _| s.import_fixture.clone().unwrap());
    fixture.set_controls(memex_client::fixture::ImportControls { instant: true, ..Default::default() });
    let (dir, pdf, notes) = import_files("folder");
    // The real platform path: Entered → Pending over the 睡眠 row → Submit → Exited.
    let row = cx.debug_bounds("group-grp_c_sleep").expect("folder row on screen").center();
    cx.simulate_event(FileDropEvent::Entered { position: row, paths: external(&[&pdf, &notes]) });
    cx.simulate_event(FileDropEvent::Pending { position: row });
    settle(cx);
    assert_eq!(shell.read_with(cx, |s, _| s.file_drag), Some(2));
    assert_eq!(shell.read_with(cx, |s, cx| s.library.read(cx).drop_hover.clone()), Some(DropSpot::Group("grp_c_sleep".into())));
    assert!(cx.debug_bounds("file-drop-hint").is_some(), "the drop hint covers the window");
    cx.simulate_event(FileDropEvent::Submit { position: row });
    cx.simulate_event(FileDropEvent::Exited);
    pump_imports(cx);
    assert_eq!(shell.read_with(cx, |s, _| s.file_drag), None);
    assert!(cx.debug_bounds("file-drop-hint").is_none());
    let rows = tray_rows(&shell, cx);
    // The folder is expanded recursively, in name order.
    assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["synthetic.pdf", "a.md", "b.txt"]);
    assert!(rows.iter().all(|r| r.destination.group_id() == Some("grp_c_sleep")), "{rows:?}");
    assert!(rows.iter().all(|r| matches!(r.state, RowState::Done { .. })), "{rows:?}");
    assert_eq!(fixture.submits().len(), 3);
    // Clicking a finished row opens the new item.
    let RowState::Done { item_id } = rows[0].state.clone() else { unreachable!() };
    let at = cx.debug_bounds("import-row-1").expect("tray row on screen").center();
    cx.simulate_click(at, Modifiers::default());
    settle(cx);
    assert_eq!(shell.read_with(cx, |s, cx| s.metadata.read(cx).item_id().map(str::to_string)), Some(item_id));
    std::fs::remove_dir_all(&dir).ok();
}

#[::core::prelude::v1::test]
fn finder_files_dropped_on_the_reader_go_to_the_current_place_or_inbox() {
    let mut cx = TestAppContext::single();
    let path = layout_path("import-current");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let fixture = shell.read_with(cx, |s, _| s.import_fixture.clone().unwrap());
    fixture.set_controls(memex_client::fixture::ImportControls { instant: true, ..Default::default() });
    let (dir, pdf, _) = import_files("current");
    let reader = point(px(700.), px(420.));
    let drop_at = |cx: &mut VisualTestContext| {
        cx.simulate_event(FileDropEvent::Entered { position: reader, paths: external(&[&pdf]) });
        cx.simulate_event(FileDropEvent::Pending { position: reader });
        cx.simulate_event(FileDropEvent::Submit { position: reader });
        cx.simulate_event(FileDropEvent::Exited);
        pump_imports(cx);
    };
    // Start-up shows Inbox (a virtual view): the file goes to Inbox.
    drop_at(cx);
    // A folder selected: it goes into that folder.
    shell.update(cx, |s, cx| s.library.update(cx, |l, cx| l.set_filter(memex_library::model::Filter::Group("grp_c_rag".into()), cx)));
    settle(cx);
    drop_at(cx);
    let rows = tray_rows(&shell, cx);
    assert_eq!(rows.iter().map(|r| r.destination.group_id()).collect::<Vec<_>>(), [None, Some("grp_c_rag")]);
    assert_eq!(rows[1].destination.label(), "AI 研究 / 大语言模型 / RAG");
    assert_eq!(fixture.submits().len(), 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[::core::prelude::v1::test]
fn without_the_import_capability_a_drop_only_says_so_and_sends_nothing() {
    let mut cx = TestAppContext::single();
    let path = layout_path("import-closed");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let fixture = shell.read_with(cx, |s, _| s.import_fixture.clone().unwrap());
    fixture.set_controls(memex_client::fixture::ImportControls { capability_missing: true, instant: true, ..Default::default() });
    shell.update(cx, |s, cx| s.importer.update(cx, |i, cx| i.probe(cx)));
    settle(cx);
    let (dir, pdf, notes) = import_files("closed");
    let reader = point(px(700.), px(420.));
    cx.simulate_event(FileDropEvent::Entered { position: reader, paths: external(&[&pdf, &notes]) });
    cx.simulate_event(FileDropEvent::Pending { position: reader });
    settle(cx);
    assert!(cx.debug_bounds("file-drop-hint").is_some());
    cx.simulate_event(FileDropEvent::Submit { position: reader });
    cx.simulate_event(FileDropEvent::Exited);
    pump_imports(cx);
    let (rows, notice) = shell.read_with(cx, |s, cx| { let t = &s.importer.read(cx).tray; (t.rows().len(), t.notice.clone()) });
    assert_eq!(rows, 0);
    assert_eq!(notice.as_deref(), Some("Mac 端暂未开放文件导入"));
    assert_eq!((fixture.submits().len(), fixture.status_calls()), (0, 0));
    assert!(cx.debug_bounds("import-tray").is_some(), "the tray shows the notice");
    std::fs::remove_dir_all(&dir).ok();
}

#[::core::prelude::v1::test]
fn the_capture_card_under_the_list_shows_url_captures_and_hides_when_idle_or_unsupported() {
    use memex_client::capture_jobs::synthetic_capture_jobs;
    let mut cx = TestAppContext::single();
    let path = layout_path("capture-card");
    let (shell, cx) = open_shell(&mut cx, &path, SHOWCASE_ITEM);
    let fixture = shell.read_with(cx, |s, _| s.fixture.clone().unwrap());
    // Idle queue: probed, nothing shown.
    assert!(fixture.capture_job_calls() >= 1);
    assert!(cx.debug_bounds("import-tray").is_none(), "no captures and no imports: hidden");
    let poll = |shell: &Entity<Shell>, cx: &mut VisualTestContext| {
        shell.update(cx, |s, cx| s.importer.update(cx, |i, cx| i.poll_capture(cx)));
        settle(cx);
    };
    let now = memex_library::import_view::now_seconds();
    fixture.set_capture_jobs(synthetic_capture_jobs(now));
    poll(&shell, cx);
    // Collapsed: one summary line under the item list, no rows.
    let card = cx.debug_bounds("import-tray").expect("card shown");
    let list = shell.read_with(cx, |s, cx| s.importer.read(cx).card_summary(now));
    assert_eq!((list.running, list.waiting, list.needs_user, list.failed, list.done), (1, 5, 1, 1, 2));
    assert!(card.origin.x > px(900.), "docked in the right-hand list column: {card:?}");
    assert!(cx.debug_bounds("capture-row-job_synthetic_07").is_none());
    // While something runs it is read every few seconds.
    let calls = fixture.capture_job_calls();
    cx.executor().advance_clock(memex_library::capture::ACTIVE_POLL);
    settle(cx);
    assert!(fixture.capture_job_calls() > calls);
    // Expanded: every job, the failed and needs-attention ones included.
    let header = cx.debug_bounds("capture-card-header").unwrap().center();
    cx.simulate_click(header, Modifiers::default());
    settle(cx);
    for id in ["capture-row-job_synthetic_07", "capture-row-job_synthetic_05", "capture-row-job_synthetic_06", "capture-row-job_synthetic_04"] {
        assert!(cx.debug_bounds(id).is_some(), "{id}");
    }
    // An agent without the capability: hidden, and nothing more is sent.
    fixture.set_controls(FixtureControls { no_capture_jobs: true, ..fixture.controls() });
    shell.update(cx, |s, cx| s.refresh_capabilities(cx));
    settle(cx);
    assert!(cx.debug_bounds("import-tray").is_none());
    let calls = fixture.capture_job_calls();
    cx.executor().advance_clock(memex_library::capture::IDLE_POLL * 2);
    settle(cx);
    assert_eq!(fixture.capture_job_calls(), calls);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[::core::prelude::v1::test]
fn the_table_shows_publication_and_duration_columns_and_the_reader_its_dates_line() {
    let mut cx = TestAppContext::single();
    let path = layout_path("dates");
    let (shell, cx) = open_shell(&mut cx, &path, memex_client::fixture::PODCAST_ITEM);
    shell.update(cx, |s, cx| s.set_view(ViewMode::Classic, cx));
    settle(cx);
    let table = shell.read_with(cx, |s, cx| s.table.read(cx).table.clone());
    let keys = table.read_with(cx, |t, _| t.delegate().shown_keys());
    assert_eq!(&keys[keys.len() - 3..], ["added_at", "published_at", "duration"]);
    let library = shell.read_with(cx, |s, _| s.library.clone());
    // The podcast is filed in a folder and past the first window: list
    // everything, load it, then pick it again.
    library.update(cx, |l, cx| l.set_filter(memex_library::model::Filter::All, cx));
    settle(cx);
    library.update(cx, |l, cx| l.ensure_rows(0..128, cx));
    settle(cx);
    shell.update(cx, |s, cx| s.select(memex_client::fixture::PODCAST_ITEM.into(), false, cx));
    settle(cx);
    let podcast = library.read_with(cx, |l, _| l.row_for(memex_client::fixture::PODCAST_ITEM)).unwrap();
    assert_eq!((memex_library::view::cell_text(&podcast, "published_at"), memex_library::view::cell_text(&podcast, "duration")), ("2026-10-30".into(), "1:00:26".into()));
    // Optional: hiding 时长 leaves 发布日期; only the optional columns toggle.
    table.update(cx, |t, cx| { t.delegate_mut().toggle_column(8); t.delegate_mut().toggle_column(0); t.refresh(cx); });
    let keys = table.read_with(cx, |t, _| t.delegate().shown_keys());
    assert_eq!((keys.first().copied(), keys.last().copied(), keys.len()), (Some("title"), Some("published_at"), 8));
    table.update(cx, |t, cx| { t.delegate_mut().toggle_column(8); t.refresh(cx); });
    assert_eq!(table.read_with(cx, |t, _| t.delegate().shown_keys()).last().copied(), Some("duration"));
    let reader = shell.read_with(cx, |s, _| s.reader.clone());
    assert_eq!(reader.read_with(cx, |r, _| r.dates_line(memex_client::fixture::PODCAST_ITEM)).as_deref(), Some("发布 2026-10-30 · 1:00:26"));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

fn interaction_point(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} on screen")).center()
}

fn interaction_drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>, modifiers: Modifiers) {
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for i in 1..=8 {
        cx.simulate_mouse_move(from + (to - from) * (i as f32 / 8.), MouseButton::Left, modifiers);
    }
    cx.simulate_mouse_up(to, MouseButton::Left, modifiers);
    settle(cx);
}

#[::core::prelude::v1::test]
fn interaction_context_actions_keep_the_whole_selection_even_over_other_rows() {
    use memex_library::model::Filter;
    for action in ["picker-move", "picker-add", "picker-map"] {
        let mut app = TestAppContext::single();
        let path = layout_path(action);
        let (shell, cx) = open_shell(&mut app, &path, CHINESE_ITEM);
        let library = shell.read_with(cx, |s, _| s.library.clone());
        library.update(cx, |l, cx| { l.set_include_descendants(false, cx); l.set_filter(Filter::Group("grp_c_sleep".into()), cx); }); settle(cx);
        let p = interaction_point(cx, "item-row-0"); click(cx, p);
        let p = interaction_point(cx, "item-row-1"); cx.simulate_click(p, Modifiers { alt: true, ..Default::default() }); settle(cx);
        let selected = library.read_with(cx, |l, _| l.model.selection());
        // Put the popup over non-selected rows, exactly as the reported failure.
        let p = interaction_point(cx, "item-row-3");
        library.update(cx, |l, cx| l.open_picker(selected[0].clone(), p, cx)); settle(cx);
        let p = interaction_point(cx, action); click(cx, p);
        assert_eq!(library.read_with(cx, |l, _| l.model.selection()), selected, "{action} must not click through to a row");
        assert!(library.read_with(cx, |l, _| l.picker.as_ref().is_some_and(|p| p.choosing.is_some())));
        let p = interaction_point(cx, if action == "picker-map" { "picker-target-grp_c_multi" } else { "picker-target-grp_c_cog" });
        click(cx, p);
        assert!(library.read_with(cx, |l, _| l.member_notice.as_deref().unwrap_or("").contains("2 条")), "{action}: {:?}", library.read_with(cx, |l, _| l.member_notice.clone()));
    }
}

#[::core::prelude::v1::test]
fn interaction_item_keys_and_multi_drag_work_in_reading_and_classic_views() {
    use memex_library::model::Filter;
    for view in [ViewMode::Reading, ViewMode::Classic] {
        let mut app = TestAppContext::single();
        let path = layout_path(&format!("interaction-{view:?}"));
        let (shell, cx) = open_shell(&mut app, &path, CHINESE_ITEM);
        let library = shell.read_with(cx, |s, _| s.library.clone());
        let fixture = shell.read_with(cx, |s, _| s.fixture.clone().unwrap());
        fixture.set_controls(FixtureControls { group_trash: true, move_members_op: true, ..fixture.controls() });
        shell.update(cx, |s, cx| s.refresh_capabilities(cx));
        shell.update(cx, |s, cx| s.set_view(view, cx));
        library.update(cx, |l, cx| { l.set_include_descendants(false, cx); l.set_filter(Filter::Group("grp_c_sleep".into()), cx); });
        settle(cx);
        let rows = library.read_with(cx, |l, _| (0..3).map(|i| l.model.row(i).unwrap().item_id.clone()).collect::<Vec<_>>());
        let p = interaction_point(cx, "item-row-0");
        click(cx, p);
        let p = interaction_point(cx, "item-row-1");
        cx.simulate_click(p, Modifiers { alt: true, ..Default::default() }); settle(cx);
        assert_eq!(library.read_with(cx, |l, _| l.model.selection()), rows[..2], "{view:?} option selection");
        let p = interaction_point(cx, "item-row-2");
        cx.simulate_click(p, Modifiers { shift: true, ..Default::default() }); settle(cx);
        assert_eq!(library.read_with(cx, |l, _| l.model.selection()), rows, "{view:?} shift range");
        cx.simulate_keystrokes("cmd-a"); settle(cx);
        assert_eq!(library.read_with(cx, |l, _| l.model.selection_len()), library.read_with(cx, |l, _| l.model.len()));
        cx.simulate_keystrokes("cmd-shift-a"); settle(cx);
        assert_eq!(library.read_with(cx, |l, _| l.model.selection_len()), 0);
        cx.simulate_keystrokes("cmd-a escape"); settle(cx);
        assert_eq!(library.read_with(cx, |l, _| l.model.selection_len()), 0, "{view:?}: Escape clears the same selection as cmd-shift-a");
        // Return does nothing on no selection or multi-selection.
        cx.simulate_keystrokes("enter"); settle(cx);
        assert!(library.read_with(cx, |l, _| l.renaming_item().is_none()));
        let p = interaction_point(cx, "item-row-0"); click(cx, p);
        let p = interaction_point(cx, "item-row-1");
        cx.simulate_click(p, Modifiers { alt: true, ..Default::default() }); settle(cx);
        cx.simulate_keystrokes("enter"); settle(cx);
        assert!(library.read_with(cx, |l, _| l.renaming_item().is_none()));
        let from = interaction_point(cx, "item-row-0");
        let to = interaction_point(cx, "group-grp_c_cog");
        interaction_drag(cx, from, to, Modifiers::default());
        assert!(library.read_with(cx, |l, _| l.member_notice.as_deref().unwrap_or("").contains("2 条")), "{view:?}: {:?}", library.read_with(cx, |l, _| l.member_notice.clone()));
        // The same two selected rows are moved out of the source by ⌘ at drop.
        let from = interaction_point(cx, "item-row-0");
        let to = interaction_point(cx, "group-grp_c_cog");
        interaction_drag(cx, from, to, Modifiers { platform: true, ..Default::default() });
        let remaining = library.read_with(cx, |l, _| (0..l.model.len()).filter_map(|i| l.model.row(i)).map(|r| r.item_id.clone()).collect::<Vec<_>>());
        assert!(!remaining.contains(&rows[0]) && !remaining.contains(&rows[1]), "{view:?}: cmd move removes both source memberships");
        library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_cog".into()), cx)); settle(cx);
        assert!(library.read_with(cx, |l, _| rows[..2].iter().all(|id| (0..l.model.len()).any(|i| l.model.row(i).is_some_and(|r| &r.item_id == id)))));
        let p = interaction_point(cx, "item-row-0"); click(cx, p);
        let rename = library.read_with(cx, |l, _| l.model.selection()[0].clone());
        cx.simulate_keystrokes("enter"); settle(cx);
        assert_eq!(library.read_with(cx, |l, _| l.renaming_item().map(str::to_string)), Some(rename.clone()), "{view:?}: Return edits one item");
        cx.simulate_keystrokes("cmd-a backspace"); cx.simulate_input("单条合成改名"); cx.simulate_keystrokes("enter"); settle(cx);
        assert_eq!(Client::new(fixture).item_read(&rename).unwrap().title, "单条合成改名");
    }
}

#[::core::prelude::v1::test]
fn interaction_return_rename_and_soft_trash_preserve_canonical_content() {
    use memex_library::model::Filter;
    let mut app = TestAppContext::single();
    let path = layout_path("interaction-rename-trash");
    let (shell, cx) = open_shell(&mut app, &path, CHINESE_ITEM);
    let library = shell.read_with(cx, |s, _| s.library.clone());
    let fixture = shell.read_with(cx, |s, _| s.fixture.clone().unwrap());
    fixture.set_controls(FixtureControls { group_trash: true, ..fixture.controls() });
    shell.update(cx, |s, cx| s.refresh_capabilities(cx));
    let client = Client::new(fixture);
    library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_sleep".into()), cx)); settle(cx);
    let p = interaction_point(cx, "item-row-0"); click(cx, p);
    let id = library.read_with(cx, |l, _| l.model.selection()[0].clone());
    let before = client.item_read(&id).unwrap();
    cx.simulate_keystrokes("enter"); settle(cx);
    assert_eq!(library.read_with(cx, |l, _| l.renaming_item().map(str::to_string)), Some(id.clone()));
    // Cmd-A / Backspace in the name field must not select or trash the whole list.
    cx.simulate_keystrokes("cmd-a backspace"); cx.simulate_input("合成重命名"); cx.simulate_keystrokes("enter"); settle(cx);
    let after = client.item_read(&id).unwrap();
    assert_eq!(after.title, "合成重命名");
    assert_eq!(after.markdown, before.markdown);
    let (mut old_tags, mut new_tags) = (before.manual_tags.unwrap(), after.manual_tags.unwrap());
    old_tags.sort(); new_tags.sort();
    assert_eq!(new_tags, old_tags);
    assert!(library.read_with(cx, |l, _| l.renaming_item().is_none()));
    let p = interaction_point(cx, "item-row-0"); click(cx, p);
    let ids = library.read_with(cx, |l, _| { let mut ids = l.model.selection(); ids.push(l.model.row(1).unwrap().item_id.clone()); ids });
    let p = interaction_point(cx, "item-row-1"); cx.simulate_click(p, Modifiers { alt: true, ..Default::default() }); settle(cx);
    let from = interaction_point(cx, "item-row-0"); let to = interaction_point(cx, "trash-grp_z_psych");
    interaction_drag(cx, from, to, Modifiers::default());
    let entries = client.group_trash_all("grp_z_psych").unwrap();
    assert_eq!(entries.len(), 2);
    assert!(ids.iter().all(|id| entries.iter().any(|e| &e.item_id == id)));
    library.update(cx, |l, cx| l.show_group_trash("grp_z_psych".into(), cx)); settle(cx);
    for id in &ids { library.update(cx, |l, cx| l.restore_from_trash(id, cx)); settle(cx); }
    assert!(client.group_trash_all("grp_z_psych").unwrap().is_empty());
}

#[::core::prelude::v1::test]
fn interaction_tree_modifiers_return_delete_and_trash_drop_use_recoverable_nodes() {
    let mut app = TestAppContext::single();
    let path = layout_path("interaction-tree");
    let (shell, cx) = open_shell(&mut app, &path, CHINESE_ITEM);
    let (tree, library) = shell.read_with(cx, |s, cx| (s.sidebar.read(cx).tree(), s.library.clone()));
    let p = interaction_point(cx, "group-grp_c_cog"); click(cx, p);
    let p = interaction_point(cx, "group-grp_c_sleep"); cx.simulate_click(p, Modifiers { shift: true, ..Default::default() }); settle(cx);
    let mut selected = tree.read_with(cx, |t, cx| t.selection(cx)); selected.sort();
    assert_eq!(selected, ["grp_c_cog", "grp_c_sleep"]);
    let p = interaction_point(cx, "group-grp_c_sleep"); cx.simulate_click(p, Modifiers { alt: true, ..Default::default() }); settle(cx);
    assert_eq!(tree.read_with(cx, |t, cx| t.selection(cx)), ["grp_c_cog"]);
    cx.simulate_keystrokes("cmd-a"); settle(cx);
    assert!(tree.read_with(cx, |t, cx| t.selection(cx).len()) > 3);
    cx.simulate_keystrokes("escape"); settle(cx);
    assert!(tree.read_with(cx, |t, cx| t.selection(cx).is_empty()));
    let p = interaction_point(cx, "group-grp_c_sleep"); click(cx, p);
    cx.simulate_keystrokes("enter"); settle(cx);
    assert_eq!(tree.read_with(cx, |t, _| t.renaming().map(str::to_string)), Some("grp_c_sleep".into()));
    cx.simulate_keystrokes("cmd-a backspace"); cx.simulate_input("合成睡眠"); cx.simulate_keystrokes("enter"); settle(cx);
    assert_eq!(library.read_with(cx, |l, _| l.model.group("grp_c_sleep").unwrap().name.clone()), "合成睡眠", "editor={:?}, draft={:?}", library.read_with(cx, |l, _| l.editor.message().map(str::to_string)), tree.read_with(cx, |t, _| t.renaming().map(str::to_string)));
    assert!(library.read_with(cx, |l, _| l.delete.is_none()), "Backspace edited only the input");
    let from = interaction_point(cx, "group-grp_c_sleep"); let to = interaction_point(cx, "trash");
    interaction_drag(cx, from, to, Modifiers::default());
    assert!(matches!(library.read_with(cx, |l, _| l.delete.clone()), Some(memex_library::view::DeleteFlow::Confirm { .. })));
    library.update(cx, |l, cx| l.confirm_delete(cx)); settle(cx);
    assert!(library.read_with(cx, |l, _| l.model.group("grp_c_sleep").is_none()));
    let deletion = library.read_with(cx, |l, _| l.deletions.iter().find(|d| d.name == "合成睡眠").unwrap().deletion_id.clone());
    library.update(cx, |l, cx| l.restore_deletion(deletion, cx)); settle(cx);
    assert!(library.read_with(cx, |l, _| l.model.group("grp_c_sleep").is_some()));
    let p = interaction_point(cx, "group-grp_u_mine"); click(cx, p); cx.simulate_keystrokes("backspace"); settle(cx);
    assert!(matches!(library.read_with(cx, |l, _| l.delete.clone()), Some(memex_library::view::DeleteFlow::Confirm { .. })));
}

#[::core::prelude::v1::test]
fn interaction_folder_multi_drag_maps_snapshots_and_command_moves_original_ids() {
    for view in [ViewMode::Reading, ViewMode::Classic] {
        let mut app = TestAppContext::single();
        let path = layout_path("folder-drag");
        let (shell, cx) = open_shell(&mut app, &path, CHINESE_ITEM);
        let (fixture, library) = shell.read_with(cx, |s, _| (s.fixture.clone().unwrap(), s.library.clone()));
        fixture.set_controls(FixtureControls { transfer_folders_op: true, ..fixture.controls() });
        shell.update(cx, |s, cx| { s.set_view(view, cx); s.refresh_capabilities(cx); s.library.update(cx, |l, cx| l.reload(cx)); });
        settle(cx);
        let before = library.read_with(cx, |l, _| l.model.groups().len());
        let p = interaction_point(cx, "group-grp_c_cog"); click(cx, p);
        let p = interaction_point(cx, "group-grp_c_sleep"); cx.simulate_click(p, Modifiers { alt: true, ..Default::default() }); settle(cx);
        let from = interaction_point(cx, "group-grp_c_cog");
        let to = interaction_point(cx, "group-grp_u_mine");
        interaction_drag(cx, from, to, Modifiers::default());
        let groups = library.read_with(cx, |l, _| l.model.groups().to_vec());
        assert_eq!(groups.len(), before + 6, "two top folders and cog's four descendants; {:?}", library.read_with(cx, |l, _| l.member_notice.clone()));
        assert_eq!(groups.iter().find(|n| n.id == "grp_c_cog").unwrap().parent_id.as_deref(), Some("grp_z_psych"));
        let copied = groups.iter().find(|n| n.name == "认知" && n.id != "grp_c_cog").unwrap();
        assert_eq!(copied.parent_id.as_deref(), Some("grp_u_mine"));
        assert!(groups.iter().any(|n| n.name == "认知偏差" && n.parent_id.as_deref() == Some(copied.id.as_str())));
        assert!(library.read_with(cx, |l, _| l.member_notice.as_deref().unwrap_or("").contains("不会自动同步")));

        // Reselect only the original folder. ⌘ is pressed during the drag,
        // not on mouse-down; it must move, not toggle selection or map again.
        let p = interaction_point(cx, "group-grp_c_sleep"); click(cx, p);
        let from = interaction_point(cx, "group-grp_c_sleep");
        let to = interaction_point(cx, "group-grp_u_mine");
        interaction_drag(cx, from, to, Modifiers { platform: true, ..Default::default() });
        assert_eq!(library.read_with(cx, |l, _| l.model.groups().len()), before + 6);
        assert_eq!(library.read_with(cx, |l, _| l.model.group("grp_c_sleep").unwrap().parent_id.clone()).as_deref(), Some("grp_u_mine"));
    }
}

#[::core::prelude::v1::test]
fn interaction_folder_unknown_result_retries_the_original_request_without_duplicate_snapshots() {
    for internal_error in [false, true] {
    let mut app = TestAppContext::single();
    let path = layout_path("folder-unknown");
    let (shell, cx) = open_shell(&mut app, &path, CHINESE_ITEM);
    let (fixture, library) = shell.read_with(cx, |s, _| (s.fixture.clone().unwrap(), s.library.clone()));
    fixture.set_controls(FixtureControls { transfer_folders_op: true, lose_next_folder_reply: !internal_error, folder_commit_error: internal_error, ..fixture.controls() });
    shell.update(cx, |s, cx| { s.refresh_capabilities(cx); s.library.update(cx, |l, cx| l.reload(cx)); }); settle(cx);
    let p = interaction_point(cx, "group-grp_c_sleep"); click(cx, p);
    let from = interaction_point(cx, "group-grp_c_sleep"); let to = interaction_point(cx, "group-grp_u_mine");
    interaction_drag(cx, from, to, Modifiers::default());
    let pending = library.read_with(cx, |l, _| l.editor.pending_transfer()).expect("lost committed reply remains pending");
    let client = shell.read_with(cx, |s, _| s.client.clone());
    assert_eq!(client.groups().unwrap().len(), 16);
    // A refresh cannot authorize a second mapping with a fresh identity.
    library.update(cx, |l, cx| l.reload(cx)); settle(cx);
    let from = interaction_point(cx, "group-grp_c_sleep"); let to = interaction_point(cx, "group-grp_c_multi");
    interaction_drag(cx, from, to, Modifiers::default());
    assert_eq!(library.read_with(cx, |l, _| l.editor.pending_transfer()), Some(pending));
    assert_eq!(client.groups().unwrap().len(), 16);
    let p = interaction_point(cx, "retry-folder-transfer"); click(cx, p);
    assert!(library.read_with(cx, |l, _| l.editor.pending_transfer().is_none()));
    assert_eq!(client.groups().unwrap().len(), 16);
    assert_eq!(library.read_with(cx, |l, _| l.model.groups().iter().filter(|g| g.name == "睡眠").count()), 2);
    }
}
